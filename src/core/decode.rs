extern crate ffmpeg_next as ffmpeg;

use ffmpeg::codec::decoder::Video as AvDecoder;
use ffmpeg::codec::Context as AvContext;
use ffmpeg::format::pixel::Pixel as AvPixel;
use ffmpeg::software::scaling::{context::Context as AvScaler, flag::Flags as AvScalerFlags};
use ffmpeg::util::error::EAGAIN;
use ffmpeg::{Error as AvError, Rational as AvRational};

use crate::core::error::Error;
use crate::core::ffi;
use crate::core::ffi_hwaccel;
#[cfg(feature = "ndarray")]
use crate::core::frame::Frame;
use crate::core::frame::{RawFrame, FRAME_PIXEL_FORMAT};
use crate::core::hwaccel::{HardwareAccelerationContext, HardwareAccelerationDeviceType};
use crate::core::io::{Reader, ReaderBuilder};
use crate::core::location::Location;
use crate::core::options::Options;
use crate::core::packet::Packet;
use crate::core::resize::Resize;
use crate::core::time::Time;

type Result<T> = std::result::Result<T, Error>;

/// 硬件加速时总是使用 NV12 像素格式，稍后再进行缩放。
static HWACCEL_PIXEL_FORMAT: AvPixel = AvPixel::NV12;

/// 解码器构建器，用于配置和创建解码器。
pub struct DecoderBuilder<'a> {
    /// 解码器输入源。
    source: Location,
    // 解码器选项。
    options: Option<&'a Options>,
    // 缩放策略。
    resize: Option<Resize>,
    // 硬件加速设备类型。
    hardware_acceleration_device_type: Option<HardwareAccelerationDeviceType>,
}

impl<'a> DecoderBuilder<'a> {
    /// 创建一个新的解码器构建器。
    /// * `source` - 要解码的源。
    pub fn new(source: impl Into<Location>) -> Self {
        Self {
            source: source.into(),
            options: None,
            resize: None,
            hardware_acceleration_device_type: None,
        }
    }

    /// 设置自定义选项。
    ///
    /// * `options` - 自定义选项。
    pub fn with_options(mut self, options: &'a Options) -> Self {
        self.options = Some(options);
        self
    }

    /// 设置帧的缩放。
    ///
    /// * `resize` - 要应用的缩放。
    pub fn with_resize(mut self, resize: Resize) -> Self {
        self.resize = Some(resize);
        self
    }

    /// 启用硬件加速。
    ///
    /// * `device_type` - 硬件加速设备类型。
    pub fn with_hardware_acceleration(
        mut self,
        device_type: HardwareAccelerationDeviceType,
    ) -> Self {
        self.hardware_acceleration_device_type = Some(device_type);
        self
    }

    /// 构建解码器。
    ///
    /// 此方法负责根据当前配置构建一个解码器实例。它首先使用`ReaderBuilder`来配置和创建一个媒体流读取器，
    /// 然后选择最佳的视频流索引，并利用这些配置来创建并返回一个`Decoder`实例。
    ///
    /// # Returns
    ///
    /// 如果构建过程成功，则返回一个`Result`类型，包含构建好的`Decoder`实例；否则返回错误。
    pub fn build(self) -> Result<Decoder> {
        // 创建ReaderBuilder实例，并初始化配置
        let mut reader_builder = ReaderBuilder::new(self.source);
        // 如果有额外的选项配置，则应用这些配置
        if let Some(options) = self.options {
            reader_builder = reader_builder.with_options(options);
        }
        // 构建配置好的媒体流读取器
        let reader = reader_builder.build()?;
        // 获取最佳的视频流索引
        let reader_stream_index = reader.best_video_stream_index()?;
        // 创建并返回Decoder实例
        Ok(Decoder {
            decoder: DecoderSplit::new(
                &reader,
                reader_stream_index,
                self.resize,
                self.hardware_acceleration_device_type,
            )?,
            reader,
            reader_stream_index,
            draining: false,
        })
    }
}

/// 解码视频文件和流。
///
/// # 示例
///
/// ```ignore
/// let decoder = Decoder::new(Path::new("video.mp4")).unwrap();
/// decoder
///     .decode_iter()
///     .take_while(Result::is_ok)
///     .for_each(|frame| println!("Got frame!"),
/// );
/// ```
pub struct Decoder {
    /// 解码器的拆分部分。
    decoder: DecoderSplit,
    // 媒体流读取器。
    reader: Reader,
    // 媒体流索引。
    reader_stream_index: usize,
    // 读取器是否正在被排空。
    draining: bool,
}

impl Decoder {
    /// 创建一个解码器以解码指定的源。
    ///
    /// # 参数
    ///
    /// * `source` - 要解码的源。
    #[inline]
    pub fn new(source: impl Into<Location>) -> Result<Self> {
        DecoderBuilder::new(source).build()
    }

    /// 获取解码器时间基。
    #[inline]
    pub fn time_base(&self) -> AvRational {
        self.decoder.time_base()
    }

    /// 解码器流的持续时间。
    /// 获取媒体文件的时长信息
    ///
    /// 本函数通过解析媒体文件中的流信息来计算和返回文件的时长
    /// 它首先尝试获取指定索引的流，如果找不到流，则返回StreamNotFound错误
    /// 如果成功获取流信息，它将使用流的duration和time_base来创建并返回一个Time对象
    ///
    /// 返回值:
    /// - Ok(Time): 包含媒体文件时长信息的Time对象
    /// - Err(AvError::StreamNotFound): 如果无法找到指定的流
    #[inline]
    pub fn duration(&self) -> Result<Time> {
        // 尝试获取指定索引的流，如果获取失败则返回StreamNotFound错误
        let reader_stream = self
            .reader
            .input
            .stream(self.reader_stream_index)
            .ok_or(AvError::StreamNotFound)?;

        // 使用流的duration和time_base创建一个Time对象，并返回
        Ok(Time::new(
            Some(reader_stream.duration()),
            reader_stream.time_base(),
        ))
    }

    /// 解码器流中的帧数。
    /// 获取媒体流中的帧数
    ///
    /// 此函数旨在从媒体流中读取并返回帧数它通过调用底层媒体流处理库来实现
    /// 如果找不到流，则返回错误如果帧数不可用或无法确定，该函数将尝试返回一个非负值
    ///
    /// # 返回值
    ///
    /// - `Ok(u64)` 包含帧数的整数如果帧数为未知或负值，将返回 `0`
    /// - `Err(AvError::StreamNotFound)` 如果指定的流不存在
    #[inline]
    pub fn frames(&self) -> Result<u64> {
        Ok(self
            .reader
            .input
            .stream(self.reader_stream_index)
            .ok_or(AvError::StreamNotFound)?
            .frames()
            .max(0) as u64)
    }

    /// 通过迭代器接口解码帧。
    ///
    /// # 示例
    ///
    /// ```ignore
    /// decoder
    ///     .decode_iter()
    ///     .take_while(Result::is_ok)
    ///     .map(Result::unwrap)
    ///     .for_each(|(ts, frame)| {
    ///         // 处理帧...
    ///     });
    /// ```
    #[cfg(feature = "ndarray")]
    pub fn decode_iter(&mut self) -> impl Iterator<Item = Result<(Time, Frame)>> + '_ {
        std::iter::from_fn(move || Some(self.decode()))
    }

    /// 解码单个帧。
    ///
    /// # 返回值
    ///
    /// 帧的时间戳（相对于流）和帧本身的元组。
    ///
    /// # 示例
    ///
    /// ```ignore
    /// loop {
    ///     let (ts, frame) = decoder.decode()?;
    ///     // 处理帧...
    /// }
    /// ```

    /// 解码视频帧
    ///
    /// 本函数尝试从输入流中读取数据，并解码为视频帧。它处理两种状态：正常读取和排干读取。
    /// 正常情况下，它会从`reader`中读取数据包并尝试解码。当`reader`无法提供更多数据包时，
    /// 函数进入排干状态，尝试解码剩余的数据。如果没有更多帧可以解码，它会返回错误。
    ///
    /// # 返回值
    ///
    /// - `Ok((Time, Frame))`: 成功解码一帧视频，返回时间和帧数据。
    /// - `Err(Error::ReadExhausted)`: 读取数据源耗尽，无法读取更多数据包。
    /// - `Err(Error::DecodeExhausted)`: 解码器耗尽，无法解码出更多帧。
    #[cfg(feature = "ndarray")]
    pub fn decode(&mut self) -> Result<(Time, Frame)> {
        Ok(loop {
            // 当不处于排干状态时，尝试从reader中读取数据包
            if !self.draining {
                let packet_result = self.reader.read(self.reader_stream_index);
                // 如果读取结果为ReadExhausted错误，说明当前数据源已耗尽，设置排干标志为true，继续下一轮循环
                if matches!(packet_result, Err(Error::ReadExhausted)) {
                    self.draining = true;
                    continue;
                }
                // 成功读取数据包，尝试解码为帧
                let packet = packet_result?;
                if let Some(frame) = self.decoder.decode(packet)? {
                    break frame;
                }
            // 当处于排干状态时，尝试从解码器中排出剩余帧
            } else if let Some(frame) = self.decoder.drain()? {
                break frame;
            // 如果解码器中没有更多帧，且无法继续读取或排出帧时，返回DecodeExhausted错误
            } else {
                return Err(Error::DecodeExhausted);
            }
        })
    }

    /// 通过迭代器接口解码帧。类似于 `decode_raw`，但通过无限迭代器返回帧。
    pub fn decode_raw_iter(&mut self) -> impl Iterator<Item = Result<RawFrame>> + '_ {
        std::iter::from_fn(move || Some(self.decode_raw()))
    }

    /// 解码的原始帧作为 [`RawFrame`]。
    ///
    /// 本函数尝试从输入流中读取数据，并解码为原始帧。在正常情况下，它会不断读取数据包并尝试解码，
    /// 直到成功解码出一个原始帧。如果输入流被耗尽，则尝试通过解码器排出剩余数据来获取最后的原始帧。
    /// 如果没有更多的帧可以解码或排出，则返回错误。
    pub fn decode_raw(&mut self) -> Result<RawFrame> {
        Ok(loop {
            // 当draining标志未设置时，继续读取数据包
            if !self.draining {
                let packet_result = self.reader.read(self.reader_stream_index);
                // 如果读取结果为ReadExhausted错误，表示输入流已被耗尽，设置draining标志以开始排出操作
                if matches!(packet_result, Err(Error::ReadExhausted)) {
                    self.draining = true;
                    continue;
                }
                let packet = packet_result?;
                // 尝试解码数据包为原始帧，如果成功则跳出循环返回帧
                if let Some(frame) = self.decoder.decode_raw(packet)? {
                    break frame;
                }
            } else if let Some(frame) = self.decoder.drain_raw()? {
                // 如果draining标志已设置，则尝试通过排出操作获取剩余的原始帧，如果成功则跳出循环返回帧
                break frame;
            } else {
                // 如果没有更多的帧可以解码或排出，则返回DecodeExhausted错误
                return Err(Error::DecodeExhausted);
            }
        })
    }

    /// 在读取器中查找。
    ///
    /// 有关更多信息，请参见 [`Reader::seek`](crate::io::Reader::seek)。
    /// 在音频流中寻求到指定的时间戳位置
    ///
    /// 此函数允许用户在音频流中非线性地移动到特定时间点，通过提供一个时间戳（以毫秒为单位）。
    /// 它首先使用 `self.reader.seek` 方法移动到接近指定时间戳的位置，然后调用 `self.decoder.decoder.flush()`
    /// 以确保解码器状态得到正确更新，准备从新位置开始解码。
    ///
    /// # 参数
    /// * `timestamp_milliseconds` - 指定的时间戳，以毫秒为单位。这表示用户想要在音频流中达到的位置。
    ///
    /// # 返回值
    /// 如果寻求操作成功，返回 `Ok(())`；如果发生错误，返回一个描述错误的 `Result` 类型。
    #[inline]
    pub fn seek(&mut self, timestamp_milliseconds: i64) -> Result<()> {
        // 调用底层的 seek 方法来移动到接近指定时间戳的位置，并在寻求后刷新解码器状态
        self.reader
            .seek(timestamp_milliseconds)
            .inspect(|_| self.decoder.decoder.flush())
    }

    /// 在读取器中查找特定帧。
    ///
    /// 有关更多信息，请参见 [`Reader::seek_to_frame`](crate::io::Reader::seek_to_frame)。
    #[inline]
    pub fn seek_to_frame(&mut self, frame_number: i64) -> Result<()> {
        self.reader
            .seek_to_frame(frame_number)
            .inspect(|_| self.decoder.decoder.flush())
    }

    /// 查找读取器的开头。
    ///
    /// 有关更多信息，请参见 [`Reader::seek_to_start`](crate::io::Reader::seek_to_start)。
    #[inline]
    pub fn seek_to_start(&mut self) -> Result<()> {
        self.reader
            .seek_to_start()
            .inspect(|_| self.decoder.decoder.flush())
    }

    /// 将解码器拆分为解码器（类型为 [`DecoderSplit`]）和 [`Reader`]。
    ///
    /// 这允许调用者将流读取与解码分离，这对于高级用例很有用。
    ///
    /// # 返回值
    ///
    /// [`DecoderSplit`]、[`Reader`] 和读取器流索引的元组。
    #[inline]
    pub fn into_parts(self) -> (DecoderSplit, Reader, usize) {
        (self.decoder, self.reader, self.reader_stream_index)
    }

    /// 获取解码器的输入大小（分辨率尺寸）：宽度和高度。
    #[inline(always)]
    pub fn size(&self) -> (u32, u32) {
        self.decoder.size
    }

    /// 获取应用缩放后的解码器输出大小（分辨率尺寸）：宽度和高度。
    #[inline(always)]
    pub fn size_out(&self) -> (u32, u32) {
        self.decoder.size_out
    }

    /// 获取解码器的输入帧率作为浮点值。
    ///
    /// 帧率表示视频每秒显示的帧数，这里通过计算帧率的分子和分母来得到具体的帧率值。
    /// 如果分母为0，则返回0.0，避免除以零的错误。
    pub fn frame_rate(&self) -> f32 {
        // 尝试获取输入流的帧率
        let frame_rate = self
            .reader
            .input
            .stream(self.reader_stream_index)
            .map(|stream| stream.rate());

        // 检查是否成功获取帧率
        if let Some(frame_rate) = frame_rate {
            // 确保帧率的分母不为零，以避免除以零的情况
            if frame_rate.denominator() > 0 {
                // 计算并返回帧率的浮点值
                (frame_rate.numerator() as f32) / (frame_rate.denominator() as f32)
            } else {
                // 如果分母为零，返回零
                0.0
            }
        } else {
            // 如果未获取到帧率，返回零
            0.0
        }
    }
}

/// 解码器和读取器的拆分部分。
///
/// 重要提示：在读取器耗尽后不要忘记排空解码器。它可能仍然包含帧。循环运行 `drain_raw()` 或 `drain()` 直到不再生成帧。
pub struct DecoderSplit {
    // 解码器上下文
    decoder: AvDecoder,
    // 解码器的时间基
    decoder_time_base: AvRational,
    // 解码器输出的帧
    hwaccel_context: Option<HardwareAccelerationContext>,
    // 解码器的输出帧
    scaler: Option<AvScaler>,
    // 解码器输出帧的格式
    size: (u32, u32),
    // 解码器输出帧的格式
    size_out: (u32, u32),
    // 解码器是否处于关闭状态
    draining: bool,
}

impl DecoderSplit {
    /// 创建新的 [`DecoderSplit`]。
    ///
    /// # 参数
    ///
    /// * `reader` - 用于初始化解码器的 [`Reader`]。
    /// 创建一个新的视频解码器实例。
    ///
    /// * `reader` - 一个引用，指向用于读取媒体流的读取器。
    /// * `reader_stream_index` - 读取器流的索引，用于指定要解码的流。
    /// * `resize` - 可选的缩放策略，如果提供，则使用该策略对输出进行缩放。
    /// * `hwaccel_device_type` - 可选的硬件加速设备类型，如果提供，则使用相应的硬件加速。
    pub fn new(
        reader: &Reader,
        reader_stream_index: usize,
        resize: Option<Resize>,
        hwaccel_device_type: Option<HardwareAccelerationDeviceType>,
    ) -> Result<Self> {
        // 获取指定索引的流，如果不存在则返回错误。
        let reader_stream = reader
            .input
            .stream(reader_stream_index)
            .ok_or(AvError::StreamNotFound)?;

        // 初始化解码器上下文并设置时间基。
        let mut decoder = AvContext::new();
        ffi::set_decoder_context_time_base(&mut decoder, reader_stream.time_base());
        // 设置解码器参数。
        decoder.set_parameters(reader_stream.parameters())?;

        // 根据是否提供了硬件加速设备类型，决定是否创建硬件加速上下文。
        let hwaccel_context = match hwaccel_device_type {
            Some(device_type) => Some(HardwareAccelerationContext::new(&mut decoder, device_type)?),
            None => None,
        };

        // 获取视频解码器和时间基。
        let decoder = decoder.decoder().video()?;
        let decoder_time_base = decoder.time_base();

        // 检查解码器格式、宽度和高度，如果任一值无效，则返回错误。
        if decoder.format() == AvPixel::None || decoder.width() == 0 || decoder.height() == 0 {
            return Err(Error::MissingCodecParameters);
        }

        // 根据是否提供了缩放策略，计算最终的输出尺寸。
        let (resize_width, resize_height) = match resize {
            Some(resize) => resize
                .compute_for((decoder.width(), decoder.height()))
                .ok_or(Error::InvalidResizeParameters)?,
            None => (decoder.width(), decoder.height()),
        };

        // 确定缩放器的输入格式，如果使用了硬件加速，则使用硬件加速器的像素格式，否则使用解码器的格式。
        let scaler_input_format = if hwaccel_context.is_some() {
            HWACCEL_PIXEL_FORMAT
        } else {
            decoder.format()
        };

        // 判断是否需要创建缩放器，如果输入格式和输出格式不同，或者尺寸不同，则需要。
        let is_scaler_needed = !(scaler_input_format == FRAME_PIXEL_FORMAT
            && decoder.width() == resize_width
            && decoder.height() == resize_height);
        let scaler = if is_scaler_needed {
            Some(
                AvScaler::get(
                    scaler_input_format,
                    decoder.width(),
                    decoder.height(),
                    FRAME_PIXEL_FORMAT,
                    resize_width,
                    resize_height,
                    AvScalerFlags::AREA,
                )
                .map_err(Error::BackendError)?,
            )
        } else {
            None
        };

        // 保存原始尺寸和输出尺寸。
        let size = (decoder.width(), decoder.height());
        let size_out = (resize_width, resize_height);

        // 返回新的实例。
        Ok(Self {
            decoder,
            decoder_time_base,
            hwaccel_context,
            scaler,
            size,
            size_out,
            draining: false,
        })
    }

    /// 获取解码器时间基。
    #[inline]
    pub fn time_base(&self) -> AvRational {
        self.decoder_time_base
    }

    /// 解码 [`Packet`]。
    ///
    /// 将数据包馈送到解码器并返回帧（如果有可用帧）。调用者应继续馈送数据包，直到解码器返回帧。
    ///
    /// # 返回值
    ///
    /// 如果解码器有可用帧，则返回 [`Frame`] 和时间戳（相对于流）的元组，如果没有则返回 [`None`]。
    #[cfg(feature = "ndarray")]
    pub fn decode(&mut self, packet: Packet) -> Result<Option<(Time, Frame)>> {
        match self.decode_raw(packet)? {
            Some(mut frame) => Ok(Some(self.raw_frame_to_time_and_frame(&mut frame)?)),
            None => Ok(None),
        }
    }

    /// 解码 [`Packet`]。
    ///
    /// 将数据包馈送到解码器并返回帧（如果有可用帧）。调用者应继续馈送数据包，直到解码器返回帧。
    ///
    /// # 返回值
    ///
    /// 如果解码器有可用帧，则返回解码的原始帧作为 [`RawFrame`]，如果没有则返回 [`None`]。
    pub fn decode_raw(&mut self, packet: Packet) -> Result<Option<RawFrame>> {
        assert!(!self.draining);
        self.send_packet_to_decoder(packet)?;
        self.receive_frame_from_decoder()
    }

    /// 从解码器中排出一个帧。
    ///
    /// 调用一次排空后，解码器处于排空模式，调用者可能不再使用正常解码，否则会导致恐慌。
    ///
    /// # 返回值
    ///
    /// 如果解码器有可用帧，则返回 [`Frame`] 和时间戳（相对于流）的元组，如果没有则返回 [`None`]。
    #[cfg(feature = "ndarray")]
    pub fn drain(&mut self) -> Result<Option<(Time, Frame)>> {
        match self.drain_raw()? {
            Some(mut frame) => Ok(Some(self.raw_frame_to_time_and_frame(&mut frame)?)),
            None => Ok(None),
        }
    }

    /// 从解码器中排出一个帧。
    ///
    /// 调用一次排空后，解码器处于排空模式，调用者可能不再使用正常解码，否则会导致恐慌。
    ///
    /// # 返回值
    ///
    /// 如果解码器有可用帧，则返回解码的原始帧作为 [`RawFrame`]，如果没有则返回 [`None`]。
    pub fn drain_raw(&mut self) -> Result<Option<RawFrame>> {
        if !self.draining {
            self.decoder.send_eof().map_err(Error::BackendError)?;
            self.draining = true;
        }
        self.receive_frame_from_decoder()
    }

    /// 获取解码器的输入大小（分辨率尺寸）：宽度和高度。
    #[inline(always)]
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// 获取应用缩放后的解码器输出大小（分辨率尺寸）：宽度和高度。
    #[inline(always)]
    pub fn size_out(&self) -> (u32, u32) {
        self.size_out
    }

    /// 将数据包发送到解码器。包括相应地重新缩放时间戳。
    ///
    /// # 参数
    ///
    /// * `packet` - 待发送到解码器的数据包。
    ///
    /// # 返回值
    ///
    /// 如果发送成功，则返回 Ok(())；否则返回一个错误。
    fn send_packet_to_decoder(&mut self, packet: Packet) -> Result<()> {
        // 将数据包拆分为其内部部分和时间基
        let (mut packet, packet_time_base) = packet.into_inner_parts();

        // 根据解码器的时间基重新缩放数据包的时间戳
        packet.rescale_ts(packet_time_base, self.decoder_time_base);

        // 将数据包发送到解码器，如果发送失败，则返回错误
        self.decoder
            .send_packet(&packet)
            .map_err(Error::BackendError)?;

        // 如果执行成功，返回 Ok(()) 表示操作成功
        Ok(())
    }

    /// 从解码器接收数据包。也将处理硬件加速转换和缩放。
    fn receive_frame_from_decoder(&mut self) -> Result<Option<RawFrame>> {
        // 尝试从解码器接收一帧数据
        match self.decoder_receive_frame()? {
            // 如果接收到帧数据
            Some(frame) => {
                // 根据硬件加速上下文处理帧数据
                let frame = match self.hwaccel_context.as_ref() {
                    // 如果硬件加速上下文存在且格式与帧数据格式匹配，则下载帧数据
                    Some(hwaccel_context) if hwaccel_context.format() == frame.format() => {
                        Self::download_frame(&frame)?
                    }
                    // 否则，直接使用原始帧数据
                    _ => frame,
                };

                // 根据缩放器处理帧数据
                let frame = match self.scaler.as_mut() {
                    // 如果缩放器存在，则对帧数据进行缩放
                    Some(scaler) => Self::rescale_frame(&frame, scaler)?,
                    // 否则，直接使用原始帧数据
                    _ => frame,
                };

                // 返回处理后的帧数据
                Ok(Some(frame))
            }
            // 如果没有接收到帧数据，则返回None
            None => Ok(None),
        }
    }

    /// 从解码器中提取解码后的帧。此函数还实现了重试机制，以防解码器发出 `EAGAIN` 信号。
    fn decoder_receive_frame(&mut self) -> Result<Option<RawFrame>> {
        // 初始化一个空的原始帧，用于接收解码器输出的帧数据
        let mut frame = RawFrame::empty();

        // 从解码器接收帧数据
        let decode_result = self.decoder.receive_frame(&mut frame);

        // 根据接收帧的结果进行匹配处理
        match decode_result {
            // 如果解码成功，返回包含帧的数据
            Ok(()) => Ok(Some(frame)),
            // 如果解码器达到数据末尾，返回读取耗尽错误
            Err(AvError::Eof) => Err(Error::ReadExhausted),
            // 如果解码器发出 `EAGAIN` 信号，表示暂时无法读取帧，返回 None 表示未读取到帧
            Err(AvError::Other { errno }) if errno == EAGAIN => Ok(None),
            // 其他错误情况，将错误转换为函数的错误类型并返回
            Err(err) => Err(err.into()),
        }
    }

    /// 从外部硬件加速设备下载帧。
    ///
    /// 此函数负责从硬件加速设备中下载一帧数据，并将其格式化为可用于软件处理的帧。
    /// 它首先创建一个空的RawFrame实例，然后设置其格式以匹配硬件加速设备的像素格式，
    /// 接着调用硬件设备的传输函数将帧数据传输到此RawFrame实例中，最后复制原始帧的属性到新帧。
    ///
    /// # 参数
    ///
    /// * `frame`: 一个指向原始帧的引用，该帧包含从硬件设备下载所需的信息。
    ///
    /// # 返回
    ///
    /// * `Result<RawFrame>`: 返回一个结果类型，其中包含下载并格式化后的帧，如果操作成功，
    ///   或者一个错误，如果操作失败。
    fn download_frame(frame: &RawFrame) -> Result<RawFrame> {
        // 创建一个空的帧用于接收下载的内容。
        let mut frame_downloaded = RawFrame::empty();
        // 设置帧的格式以匹配硬件加速设备的像素格式。
        frame_downloaded.set_format(HWACCEL_PIXEL_FORMAT);
        // 调用硬件设备的API将帧数据传输到我们创建的帧中。
        ffi_hwaccel::hwdevice_transfer_frame(&mut frame_downloaded, frame)?;
        // 复制原始帧的属性到新下载的帧中，以保留必要的元数据。
        ffi::copy_frame_props(frame, &mut frame_downloaded);
        // 返回下载并格式化后的帧。
        Ok(frame_downloaded)
    }

    /// 使用缩放器缩放帧。
    ///
    /// # 参数
    ///
    /// - `frame`: 指向原始帧的引用，用于缩放处理。
    /// - `scaler`: 指向一个可变的AV缩放器实例，用于执行缩放操作。
    ///
    /// # 返回
    ///
    /// 返回一个结果，包含缩放后的帧。如果缩放过程中发生错误，则返回一个错误。
    fn rescale_frame(frame: &RawFrame, scaler: &mut AvScaler) -> Result<RawFrame> {
        // 创建一个空的帧，用于存储缩放后的帧数据。
        let mut frame_scaled = RawFrame::empty();

        // 使用缩放器对原始帧进行缩放处理。如果发生错误，将错误转换为自定义错误类型并返回。
        scaler
            .run(frame, &mut frame_scaled)
            .map_err(Error::BackendError)?;

        // 复制原始帧的属性到缩放后的帧中，以保留除像素数据外的其他信息。
        ffi::copy_frame_props(frame, &mut frame_scaled);

        // 返回缩放后的帧。
        Ok(frame_scaled)
    }

    /// 将原始帧转换为时间和帧
    ///
    /// 此函数接收一个可变引用到一个 `RawFrame` 对象，并将其转换为一个包含时间和帧的元组。
    /// 时间是根据帧的 DTS（解码时间戳）计算的，而帧本身则被转换为一个 RGB24 格式的 ndarray。
    ///
    /// # 参数
    ///
    /// * `frame` - 一个指向 `RawFrame` 的可变引用，表示待转换的原始帧。
    ///
    /// # 返回值
    ///
    /// 成功时，返回一个包含 `Time` 和 `Frame` 的元组，表示转换后的时间和帧。
    /// 如果转换过程中发生错误，则返回一个错误。
    #[cfg(feature = "ndarray")]
    fn raw_frame_to_time_and_frame(&self, frame: &mut RawFrame) -> Result<(Time, Frame)> {
        // 我们在这里使用数据包 DTS（即 `frame->pkt_dts`），因为这就是编码器在为 `PTS` 字段编码时使用的。
        // 这允许我们正确地同步音频和视频。
        let timestamp = Time::new(Some(frame.packet().dts), self.decoder_time_base);

        // 将帧转换为 RGB24 格式的 ndarray。这个转换可能会失败，因此我们在这里处理错误。
        let frame = ffi::convert_frame_to_ndarray_rgb24(frame).map_err(Error::BackendError)?;

        // 返回转换后的时间和帧。
        Ok((timestamp, frame))
    }
}

impl Drop for DecoderSplit {
    fn drop(&mut self) {
        // 在放弃之前，最大调用 `decoder_receive_frame` 的次数以排空队列中仍然存在的项目。
        const MAX_DRAIN_ITERATIONS: u32 = 100;

        // 我们需要排空解码器队列中仍然存在的项目。
        if let Ok(()) = self.decoder.send_eof() {
            for _ in 0..MAX_DRAIN_ITERATIONS {
                if self.decoder_receive_frame().is_err() {
                    break;
                }
            }
        }
    }
}

unsafe impl Send for DecoderSplit {}
unsafe impl Sync for DecoderSplit {}
