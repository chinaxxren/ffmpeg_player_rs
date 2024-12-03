extern crate ffmpeg_next as ffmpeg;

use futures::{future::OptionFuture, FutureExt};

use super::player::ControlCommand;

pub struct VideoPlaybackThread {
    control_sender: smol::channel::Sender<ControlCommand>,
    packet_sender: smol::channel::Sender<ffmpeg::codec::packet::packet::Packet>,
    receiver_thread: Option<std::thread::JoinHandle<()>>,
}

impl VideoPlaybackThread {
    pub fn start(
        stream: &ffmpeg::format::stream::Stream,
        mut video_frame_callback: Box<dyn FnMut(&ffmpeg::util::frame::Video) + Send>,
    ) -> Result<Self, anyhow::Error> {
        println!("视频线程启动 - 流信息: {}", stream.duration());

        let (control_sender, control_receiver) = smol::channel::unbounded();

        let (packet_sender, packet_receiver) = smol::channel::bounded(128);

        let decoder_context = ffmpeg::codec::Context::from_parameters(stream.parameters())?;
        let mut packet_decoder = decoder_context.decoder().video()?;

        println!("视频解码器初始化完成 - {:?}", packet_decoder.format());

        let clock = StreamClock::new(stream);

        let receiver_thread =
            std::thread::Builder::new().name("video playback thread".into()).spawn(move || {
                smol::block_on(async move {
                    let packet_receiver_impl = async {
                        loop {
                            let Ok(packet) = packet_receiver.recv().await else { 
                                println!("视频包接收结束");
                                break 
                            };

                            smol::future::yield_now().await;

                            if let Err(e) = packet_decoder.send_packet(&packet) {
                                println!("发送视频包到解码器失败: {}", e);
                                continue;
                            }

                            let mut decoded_frame = ffmpeg::util::frame::Video::empty();

                            while packet_decoder.receive_frame(&mut decoded_frame).is_ok() {
                                if let Some(delay) = clock.convert_pts_to_instant(decoded_frame.pts()) {
                                    println!("视频帧延迟: {:?}", delay);
                                    smol::Timer::after(delay).await;
                                }

                                println!("解码视频帧 - PTS: {:?}, 格式: {:?}", 
                                    decoded_frame.pts(),
                                    decoded_frame.format());
                                video_frame_callback(&decoded_frame);
                            }
                        }
                    }
                    .fuse()
                    .shared();

                    let mut playing = true;

                    loop {
                        let packet_receiver: OptionFuture<_> =
                            if playing { Some(packet_receiver_impl.clone()) } else { None }.into();

                        smol::pin!(packet_receiver);

                        futures::select! {
                            _ = packet_receiver => {},
                            received_command = control_receiver.recv().fuse() => {
                                match received_command {
                                    Ok(ControlCommand::Pause) => {
                                        println!("视频播放暂停");
                                        playing = false;
                                    }
                                    Ok(ControlCommand::Play) => {
                                        println!("视频播放开始");
                                        playing = true;
                                    }
                                    Err(e) => {
                                        println!("视频控制通道关闭,{}",e);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                })
            })?;

        Ok(Self { control_sender, packet_sender, receiver_thread: Some(receiver_thread) })
    }

    pub async fn receive_packet(&self, packet: ffmpeg::codec::packet::packet::Packet) -> bool {
        match self.packet_sender.send(packet).await {
            Ok(_) => {
                println!("视频包发送成功");
                true
            },
            Err(e) => {
                println!("视频包发送失败: {}", e);
                false
            }
        }
    }

    pub async fn send_control_message(&self, message: ControlCommand) {
        println!("发送控制消息: {:?}", message);
        if let Err(e) = self.control_sender.send(message).await {
            println!("发送控制消息失败: {}", e);
        }
    }
}

impl Drop for VideoPlaybackThread {
    fn drop(&mut self) {
        println!("VideoPlaybackThread drop");
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
    fn new(stream: &ffmpeg::format::stream::Stream) -> Self {
        let time_base_seconds = stream.time_base();
        let time_base_seconds =
            time_base_seconds.numerator() as f64 / time_base_seconds.denominator() as f64;

        let start_time = std::time::Instant::now();

        Self { time_base_seconds, start_time }
    }

    fn convert_pts_to_instant(&self, pts: Option<i64>) -> Option<std::time::Duration> {
        pts.and_then(|pts| {
            let pts_since_start =
                std::time::Duration::from_secs_f64(pts as f64 * self.time_base_seconds);
            self.start_time.checked_add(pts_since_start)
        })
        .map(|absolute_pts| absolute_pts.duration_since(std::time::Instant::now()))
    }
}