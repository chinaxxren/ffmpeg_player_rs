extern crate ffmpeg_next as ffmpeg;

use crate::control::player::ControlCommand;
use futures::{future::OptionFuture, FutureExt};

pub struct VideoPlaybackThread {
    // 使用通道与线程通信
    control_sender: smol::channel::Sender<ControlCommand>,
    //
    packet_sender: smol::channel::Sender<ffmpeg::codec::packet::packet::Packet>,
    //
    receiver_thread: Option<std::thread::JoinHandle<()>>,
}

impl VideoPlaybackThread {
    // 启动视频播放线程
    pub fn start(
        // 视频流
        stream: &ffmpeg::format::stream::Stream,
        // 视频帧回调函数
        mut video_frame_callback: Box<dyn FnMut(&ffmpeg::util::frame::Video) + Send>,
    ) -> Result<Self, anyhow::Error> {
        // 创建一个无界通道，用于发送控制命令
        let (control_sender, control_receiver) = smol::channel::unbounded();

        // 创建一个有界通道，用于发送视频包，缓冲区大小为 128
        let (packet_sender, packet_receiver) = smol::channel::bounded(128);

        // 从视频流参数中创建解码器上下文
        let decoder_context = ffmpeg::codec::Context::from_parameters(stream.parameters())?;

        // 从解码器上下文中创建视频解码器
        let mut packet_decoder = decoder_context.decoder().video()?;

        // 创建一个视频时钟，用于将 PTS 转换为时间戳
        let clock = StreamClock::new(stream);

        // 创建一个线程，用于接收和处理视频包
        let receiver_thread = std::thread::Builder::new()
            .name("video playback thread".into())
            .spawn(move || {
                // 在当前线程中异步运行
                smol::block_on(async move {
                    // 创建一个异步任务，用于接收视频包并解码
                    let packet_receiver_impl = async {
                        // 循环接收视频包
                        loop {
                            // 从通道中接收视频包
                            let Ok(packet) = packet_receiver.recv().await else {
                                break;
                            };

                            // 让出当前线程的执行权
                            smol::future::yield_now().await;

                            // 将视频包发送到解码器
                            packet_decoder.send_packet(&packet).unwrap();

                            // 创建一个空的视频帧
                            let mut decoded_frame = ffmpeg::util::frame::Video::empty();

                            // 循环解码视频帧
                            while packet_decoder.receive_frame(&mut decoded_frame).is_ok() {
                                // 如果视频帧有 PTS，则将其转换为时间戳
                                if let Some(delay) =
                                    clock.convert_pts_to_instant(decoded_frame.pts())
                                {
                                    // 等待指定的时间
                                    smol::Timer::after(delay).await;
                                }

                                // 调用视频帧回调函数
                                video_frame_callback(&decoded_frame);
                            }
                        }
                    }
                    .fuse() // 将异步任务合并到当前任务中
                    .shared(); // 将异步任务转换为共享任务

                    // 初始化播放状态为 true
                    let mut playing = true;

                    // 循环处理控制命令和视频包
                    loop {
                        // 根据播放状态选择是否接收视频包
                        let packet_receiver: OptionFuture<_> = if playing {
                            Some(packet_receiver_impl.clone())
                        } else {
                            None
                        }
                        .into();

                        // 固定异步任务，以便在 select! 中使用
                        smol::pin!(packet_receiver);

                        // 等待任意一个异步任务完成
                        futures::select! {

                            // 忽略视频包接收的结果
                            _ = packet_receiver => {},

                            // 接收控制命令
                            received_command = control_receiver.recv().fuse() => {
                                // 处理接收到的控制命令
                                match received_command {
                                    // 如果是暂停命令，则停止播放
                                    Ok(ControlCommand::Pause) => {
                                        playing = false;
                                    }
                                    // 如果是播放命令，则开始播放
                                    Ok(ControlCommand::Play) => {
                                        playing = true;
                                    }
                                    // 如果通道关闭，则退出循环
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

        // 返回视频播放线程的实例
        Ok(Self {
            control_sender,
            packet_sender,
            receiver_thread: Some(receiver_thread),
        })
    }

    /// 异步接收音视频数据包
    ///
    /// 该函数通过异步通道接收一个音视频数据包，并尝试将其发送到解码器进行解码
    /// 主要用途是作为音视频数据流处理 pipeline 的一部分，负责将接收到的数据包传递给后续处理环节
    ///
    /// # 参数
    ///
    /// * `packet` - 一个 `ffmpeg_next::codec::packet::Packet` 类型的数据包，包含了待解码的音视频数据
    ///
    /// # 返回值
    ///
    /// * `true` - 数据包成功发送到解码器
    /// * `false` - 数据包发送失败，这通常意味着解码器已经关闭或者发生了其他类型的错误
    pub async fn receive_packet(&self, packet: ffmpeg::codec::packet::packet::Packet) -> bool {
        // 尝试通过异步通道发送数据包到解码器
        match self.packet_sender.send(packet).await {
            // 如果数据包发送成功，返回 true
            Ok(_) => return true,
            // 如果数据包发送失败，返回 false
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
    /// * 使用 `.unwrap()` 来处理发送结果，如果发送失败，程序将会 panic
    pub async fn send_control_message(&self, message: ControlCommand) {
        self.control_sender.send(message).await.unwrap();
    }
}

impl Drop for VideoPlaybackThread {
    fn drop(&mut self) {
        self.control_sender.close();
        if let Some(receiver_join_handle) = self.receiver_thread.take() {
            receiver_join_handle.join().unwrap();
        }
    }
}

struct StreamClock {
    time_base_seconds: f64,
    start_time: std::time::Instant,
}

impl StreamClock {
    fn new(stream: &ffmpeg_next::format::stream::Stream) -> Self {
        let time_base_seconds = stream.time_base();
        let time_base_seconds =
            time_base_seconds.numerator() as f64 / time_base_seconds.denominator() as f64;

        let start_time = std::time::Instant::now();

        Self {
            time_base_seconds,
            start_time,
        }
    }

    // 将 PTS（Presentation Time Stamp，显示时间戳）转换为当前时间的绝对时间戳，并返回一个时间间隔
    fn convert_pts_to_instant(&self, pts: Option<i64>) -> Option<std::time::Duration> {
        // 对 pts 进行处理，如果 pts 为 None，则返回 None
        pts.and_then(|pts| {
            // 将 pts 转换为以秒为单位的时间间隔
            let pts_since_start =
                std::time::Duration::from_secs_f64(pts as f64 * self.time_base_seconds);
            // 将 pts 时间间隔添加到流的开始时间，得到绝对时间戳
            self.start_time.checked_add(pts_since_start)
        })
        // 如果绝对时间戳计算成功，则计算它与当前时间的时间间隔
        .map(|absolute_pts| absolute_pts.duration_since(std::time::Instant::now()))
    }
}
