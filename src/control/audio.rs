extern crate ffmpeg_next as ffmpeg;

use std::pin::Pin;

use bytemuck::Pod;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SizedSample;

use futures::future::OptionFuture;
use futures::FutureExt;
use ringbuf::ring_buffer::{RbRef, RbWrite};
use ringbuf::HeapRb;
use std::future::Future;

use crate::control::player::ControlCommand;

/// 音频播放线程结构体
///
/// 该结构体用于管理音频播放线程，其中包括两个通道发送端和一个接收线程句柄
/// 它允许用户向播放线程发送控制命令和音频数据包，同时也能够启动和停止音频播放线程
pub struct AudioPlaybackThread {
    /// 控制命令发送端
    ///
    /// 用于向播放线程发送控制命令，比如播放、暂停等
    control_sender: smol::channel::Sender<ControlCommand>,

    /// 音频数据包发送端
    ///
    /// 用于向播放线程发送音频数据包，这些数据包包含了待播放的音频数据
    packet_sender: smol::channel::Sender<ffmpeg::codec::packet::packet::Packet>,

    /// 接收线程句柄
    ///
    /// 该字段保存了接收线程的句柄，通过该句柄可以等待接收线程结束或对其执行其他控制操作
    /// 句柄的类型为标准库中的线程JoinHandle，它代表了一个可等待的线程执行环境
    receiver_thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioPlaybackThread {
    pub fn start(stream: &ffmpeg::format::stream::Stream) -> Result<Self, anyhow::Error> {
        // 创建一个用于控制音频播放的通道，用于发送控制命令和音频数据包
        let (control_sender, control_receiver) = smol::channel::unbounded();

        // 创建一个用于接收音频数据包的通道，用于接收解码后的音频数据
        let (packet_sender, packet_receiver) = smol::channel::bounded(128);

        // 初始化音频解码器的上下文
        let decoder_context = ffmpeg::codec::Context::from_parameters(stream.parameters())?;

        // 初始化音频解码器的上下文参数，包括采样格式、采样率、通道数等
        let packet_decoder = decoder_context.decoder().audio()?;

        // 获取音频输出设备的配置，包括采样率、通道数等
        let host = cpal::default_host();

        // 获取默认的音频输出设备，用于播放音频数据的播放
        let device = host
            .default_output_device()
            .expect("no output device available");

        // 获取音频输出设备的配置，包括采样率、通道数等
        let config = device.default_output_config().unwrap();

        // 创建一个音频输出流，用于播放音频数据的播放
        let receiver_thread = std::thread::Builder::new()
            .name("audio playback thread".into())
            .spawn(move || {
                // 启动音频播放线程，该线程负责从接收线程中接收音频数据包并将其播放到音频输出设备中。
                smol::block_on(async move {
                    // 获取一个音频输出设备的输出流声道。
                    let output_channel_layout = match config.channels() {
                        1 => ffmpeg::util::channel_layout::ChannelLayout::MONO,
                        2 => ffmpeg::util::channel_layout::ChannelLayout::STEREO,
                        _ => todo!(),
                    };

                    let mut ffmpeg_to_cpal_forwarder = match config.sample_format() {
                        // 8位无符号整数
                        cpal::SampleFormat::U8 => FFmpegToCPalForwarder::new::<u8>(
                            config,
                            &device,
                            packet_receiver,
                            packet_decoder,
                            ffmpeg_next::util::format::sample::Sample::U8(
                                ffmpeg_next::util::format::sample::Type::Packed,
                            ),
                            output_channel_layout,
                        ),

                        // F32位浮点数
                        cpal::SampleFormat::F32 => FFmpegToCPalForwarder::new::<f32>(
                            config,
                            &device,
                            packet_receiver,
                            packet_decoder,
                            ffmpeg_next::util::format::sample::Sample::F32(
                                ffmpeg_next::util::format::sample::Type::Packed,
                            ),
                            output_channel_layout,
                        ),

                        // 16位有符号整数
                        format @ _ => todo!("unsupported cpal output format {:#?}", format),
                    };

                    // 启动音频播放线程，该线程负责从接收线程中接收音频数据包并将其播放到音频输出设备中。
                    let packet_receiver_impl = async { ffmpeg_to_cpal_forwarder.stream().await }
                        .fuse() // 将音频数据包发送到音频输出设备中。
                        .shared(); // 等待音频数据包的接收。

                    let mut playing = true;

                    loop {
                        // 等待音频数据包的接收。
                        let packet_receiver: OptionFuture<_> = if playing {
                            Some(packet_receiver_impl.clone())
                        } else {
                            None
                        }
                        .into();

                        smol::pin!(packet_receiver);

                        futures::select! {

                            // 等待音频数据包的接收。
                            _ = packet_receiver => {},

                            // 等待控制命令的接收。
                            received_command = control_receiver.recv().fuse() => {
                                match received_command {
                                    Ok(ControlCommand::Pause) => {
                                        playing = false;
                                    }
                                    Ok(ControlCommand::Play) => {
                                        playing = true;
                                    }
                                    Err(_) => {
                                        // Channel closed -> quit
                                        return;
                                    }
                                }
                            }
                        }
                    }
                })
            })?;

        Ok(Self {
            control_sender,
            packet_sender,
            receiver_thread: Some(receiver_thread),
        })
    }

    /// 异步接收一个数据包并将其发送到内部通道中。
    ///
    /// # 参数
    ///
    /// * `packet` - 一个 `ffmpeg::codec::packet::packet::Packet` 类型的数据包，表示要发送的数据。
    ///
    /// # 返回值
    ///
    /// * 成功发送数据包后返回 `true`。
    /// * 如果发送数据包失败（例如，接收者已经关闭），则返回 `false`。
    pub async fn receive_packet(&self, packet: ffmpeg::codec::packet::packet::Packet) -> bool {
        // 尝试将数据包发送到内部通道中。
        match self.packet_sender.send(packet).await {
            // 如果发送成功，无论结果如何，都返回 true。
            Ok(_) => return true,
            // 如果发送失败，说明接收者已经关闭，返回 false。
            Err(smol::channel::SendError(_)) => return false,
        }
    }

    /// 异步发送控制消息
    ///
    /// 该函数用于将控制命令发送到一个控制通道中
    /// 它是一个异步函数，设计用于非阻塞地发送消息
    ///
    /// # 参数
    ///
    /// * `message`: ControlCommand 类型的控制命令，表示要发送的控制消息
    ///
    /// # 说明
    ///
    /// * 该函数使用 `control_sender` 来发送消息，`control_sender` 是一个异步消息发送者
    /// * 函数通过 `.await` 语法糖来等待消息发送完成
    /// * 使用 `.unwrap()` 来处理发送结果，如果发送失败，程序将会 panic（崩溃）
    ///   这种做法在实践中可能需要根据具体情况来调整，以更合理地处理错误
    pub async fn send_control_message(&self, message: ControlCommand) {
        self.control_sender.send(message).await.unwrap();
    }
}

impl Drop for AudioPlaybackThread {
    fn drop(&mut self) {
        self.control_sender.close();
        if let Some(receiver_join_handle) = self.receiver_thread.take() {
            receiver_join_handle.join().unwrap();
        }
    }
}

trait FFMpegToCPalSampleForwarder {
    /// 将音频帧转换为CPAL样本，并将其发送到内部通道中。
    /// 此方法将阻塞直到内部通道可用。
    fn forward(
        &mut self,
        audio_frame: ffmpeg_next::frame::Audio,
    ) -> Pin<Box<dyn Future<Output = ()> + '_>>;
}

// 为 ringbuf::Producer<T, R> 类型实现 FFMpegToCPalSampleForwarder 特征。
// 此实现将音频帧转换为CPAL样本，并将其发送到内部通道中。
impl<T: Pod, R: RbRef> FFMpegToCPalSampleForwarder for ringbuf::Producer<T, R>
where
    <R as RbRef>::Rb: RbWrite<T>,
{
    // 处理音频帧以供播放。此函数是异步的，并返回一个 Future。
    // 函数接收音频帧的所有权，对其进行处理并缓冲以供播放。
    //
    // 参数:
    // * `audio_frame`: 来自 ffmpeg_next 库的音频帧，表示要处理的音频数据。
    //
    // 返回:
    // * 一个 `Pin<Box<dyn Future<Output = ()> + '_>>` 对象，表示异步操作。
    fn forward(
        &mut self,
        audio_frame: ffmpeg_next::frame::Audio,
    ) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        Box::pin(async move {
            // Audio::plane() 返回错误的切片大小，因此手动修正。详见
            // 修复建议：https://github.com/zmwangx/rust-ffmpeg/pull/104。
            let expected_bytes =
                // 计算音频帧的总字节数。
                audio_frame.samples() * audio_frame.channels() as usize * core::mem::size_of::<T>();
            let cpal_sample_data: &[T] =
                // 手动转换为 T 类型的切片，以确保正确的切片大小。
                bytemuck::cast_slice(&audio_frame.data(0)[..expected_bytes]);

            // 等待直到有足够的缓冲区空间来存储音频样本。
            while self.free_len() < cpal_sample_data.len() {
                smol::Timer::after(std::time::Duration::from_millis(16)).await;
            }

            // 缓冲样本以供播放
            self.push_slice(cpal_sample_data);
        })
    }
}

struct FFmpegToCPalForwarder {
    // 音频输出流。
    _cpal_stream: cpal::Stream,

    // 音频帧到CPAL样本的管道。
    ffmpeg_to_cpal_pipe: Box<dyn FFMpegToCPalSampleForwarder>,

    // 音频帧到CPAL样本的管道。
    packet_receiver: smol::channel::Receiver<ffmpeg::codec::packet::packet::Packet>,

    // 音频帧解码器。
    packet_decoder: ffmpeg::decoder::Audio,

    // 音频帧到CPAL样本的管道。
    resampler: ffmpeg::software::resampling::Context,
}

impl FFmpegToCPalForwarder {
    // 创建一个新的音频输出实例。
    fn new<T: Send + Pod + SizedSample + 'static>(
        config: cpal::SupportedStreamConfig, // 音频配置。
        device: &cpal::Device,               // 音频输出设备。
        packet_receiver: smol::channel::Receiver<ffmpeg::codec::packet::packet::Packet>, //
        packet_decoder: ffmpeg::decoder::Audio, // 音频帧解码器。
        output_format: ffmpeg::util::format::sample::Sample, // 音频帧到CPAL样本的管道。
        output_channel_layout: ffmpeg::util::channel_layout::ChannelLayout, // 音频帧到CPAL样本的管道。
    ) -> Self {
        
        // 初始化音频帧到CPAL样本的管道。
        let buffer = HeapRb::new(4096);
        
        // 创建音频输出流。
        let (sample_producer, mut sample_consumer) = buffer.split();

        // 创建音频输出流。
        let cpal_stream = device
            .build_output_stream(
                &config.config(),
                move |data, _| {
                    let filled = sample_consumer.pop_slice(data);
                    data[filled..].fill(T::EQUILIBRIUM);
                },
                move |err| {
                    eprintln!("error feeding audio stream to cpal: {}", err);
                },
                None,
            )
            .unwrap();

        // 启动音频输出流。
        cpal_stream.play().unwrap();

        // 创建音频帧到CPAL样本的管道。
        let resampler = ffmpeg::software::resampling::Context::get(
            packet_decoder.format(),
            packet_decoder.channel_layout(),
            packet_decoder.rate(),
            output_format,
            output_channel_layout,
            config.sample_rate().0,
        )
        .unwrap();

        // 构建音频输出实例。
        Self {
            _cpal_stream: cpal_stream,
            ffmpeg_to_cpal_pipe: Box::new(sample_producer),
            packet_receiver,
            packet_decoder,
            resampler,
        }
    }

    // 异步函数用于处理音频流的接收、解码和播放。
    async fn stream(&mut self) {
        loop {
            // 等待音频数据包的接收。
            let Ok(packet) = self.packet_receiver.recv().await else {
                break;
            };

            // 向解码器发送音频数据包以进行解码。
            self.packet_decoder.send_packet(&packet).unwrap();

            // 初始化一个空的音频帧用于存储解码后的音频数据。
            let mut decoded_frame = ffmpeg::util::frame::Audio::empty();

            // 接收解码后的音频帧。
            while self
                .packet_decoder
                .receive_frame(&mut decoded_frame)
                .is_ok()
            {
                // 初始化一个空的音频帧用于存储重新采样后的音频数据。
                let mut resampled_frame = ffmpeg::util::frame::Audio::empty();

                // 对解码后的音频帧进行重新采样，以匹配播放设备的音频格式。
                self.resampler
                    .run(&decoded_frame, &mut resampled_frame)
                    .unwrap();

                // 将重新采样后的音频帧转发到播放设备。
                self.ffmpeg_to_cpal_pipe.forward(resampled_frame).await;
            }
        }
    }
}
