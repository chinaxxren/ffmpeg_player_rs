extern crate ffmpeg_next as ffmpeg;

use std::path::PathBuf;

use futures::{future::OptionFuture, FutureExt};

use crate::control::audio;
use crate::control::video;

#[derive(Clone, Copy)]
pub enum ControlCommand {
    // 播放
    Play,
    // 暂停``
    Pause,
}

pub struct PlayerControl {
    // 控制通道
    control_sender: smol::channel::Sender<ControlCommand>,
    // 数据包通道
    demuxer_thread: Option<std::thread::JoinHandle<()>>,
    // 播放状态
    playing: bool,
    // 播放状态改变回调
    playing_changed_callback: Box<dyn Fn(bool)>,
}

impl PlayerControl {
    // 启动媒体播放器的函数
    // 该函数接受一个文件路径、一个视频帧回调函数和一个播放状态改变的回调函数
    // 它会启动一个新的线程来处理媒体文件的解码和播放控制
    pub fn start(
        path: PathBuf,
        video_frame_callback: impl FnMut(&ffmpeg::util::frame::Video) + Send + 'static,
        playing_changed_callback: impl Fn(bool) + 'static,
    ) -> Result<Self, anyhow::Error> {
        // 创建控制命令发送端和接收端
        let (control_sender, control_receiver) = smol::channel::unbounded();

        // 启动解码器线程
        let demuxer_thread = std::thread::Builder::new()
            .name("demuxer thread".into())
            .spawn(move || {
                smol::block_on(async move {
                    // 打开输入文件
                    let mut input_context = ffmpeg::format::input(&path).unwrap();

                    // 查找视频流和音频流
                    let video_stream = input_context
                        .streams() // 查找所有流
                        .best(ffmpeg::media::Type::Video)
                        .unwrap();

                    // 获取视频流的索引
                    let video_stream_index = video_stream.index();

                    // 启动视频播放线程
                    let video_playback_thread = video::VideoPlaybackThread::start(
                        &video_stream,
                        Box::new(video_frame_callback),
                    )
                    .unwrap();

                    // 查找音频流和音频流的索引
                    let audio_stream = input_context
                        .streams()
                        .best(ffmpeg_next::media::Type::Audio)
                        .unwrap();

                    // 获取音频流的索引
                    let audio_stream_index = audio_stream.index();

                    // 创建音频播放线程
                    let audio_playback_thread =
                        audio::AudioPlaybackThread::start(&audio_stream).unwrap();

                    // 初始化播放状态为 true
                    let mut playing = true;

                    // 这是次优的，因为从 ffmpeg 读取数据包可能会阻塞
                    // 未来不会因此而屈服。所以虽然 ffmpeg 存在一些阻塞
                    // I/O操作，这里的调用者也会阻塞，我们不会最终轮询
                    // control_receiver 未来更进一步。
                    let packet_forwarder_impl = async {
                        for (stream, packet) in input_context.packets() {
                            if stream.index() == audio_stream_index {
                                audio_playback_thread.receive_packet(packet).await;
                            } else if stream.index() == video_stream_index {
                                video_playback_thread.receive_packet(packet).await;
                            }
                        }
                    }
                    .fuse()
                    .shared();

                    loop {
                        // 这是次优的，因为从 ffmpeg 读取数据包可能会阻塞
                        // 未来不会因此而屈服。所以虽然 ffmpeg 存在一些阻塞
                        // I/O操作，这里的调用者也会阻塞，我们不会最终轮询
                        // control_receiver 未来更进一步。
                        let packet_forwarder: OptionFuture<_> = if playing {
                            Some(packet_forwarder_impl.clone())
                        } else {
                            None
                        }
                        .into();

                        smol::pin!(packet_forwarder);

                        futures::select! {
                            // 播放完毕
                            _ = packet_forwarder => {},
                            // 从控制通道接收控制命令
                            received_command = control_receiver.recv().fuse() => {
                                match received_command {
                                    Ok(command) => {
                                        // 发送控制消息给视频播放线程和音频播放线程
                                        video_playback_thread.send_control_message(command).await;
                                        // 音频播放线程
                                        audio_playback_thread.send_control_message(command).await;

                                        match command {
                                            ControlCommand::Play => {
                                                // 继续循环，轮询数据包转发器 future 进行转发
                                                // 数据包
                                                playing = true;
                                            },
                                            ControlCommand::Pause => {
                                                playing = false;
                                            }
                                        }
                                    }
                                    Err(_) => {
                                        // 频道关闭->退出
                                        return;
                                    }
                                }
                            }
                        }
                    }
                })
            })?;

        let playing = true;
        playing_changed_callback(playing);

        Ok(Self {
            control_sender,
            demuxer_thread: Some(demuxer_thread),
            playing,
            playing_changed_callback: Box::new(playing_changed_callback),
        })
    }

    /// 切换播放状态
    ///
    /// 此函数用于暂停或恢复播放。当当前状态为播放时，将其改为暂停；
    /// 当当前状态为暂停时，将其改为播放。通过发送控制命令来实现播放和暂停的切换。
    /// 在切换状态后，将调用播放状态改变的回调函数。
    pub fn toggle_pause_playing(&mut self) {
        if self.playing {
            // 如果当前正在播放，将播放状态设置为false，并发送暂停命令
            self.playing = false;
            self.control_sender
                .send_blocking(ControlCommand::Pause)
                .unwrap();
        } else {
            // 如果当前处于暂停状态，将播放状态设置为true，并发送播放命令
            self.playing = true;
            self.control_sender
                .send_blocking(ControlCommand::Play)
                .unwrap();
        }
        // 调用播放状态改变的回调函数，通知状态已更改
        (self.playing_changed_callback)(self.playing);
    }
}

impl Drop for PlayerControl {
    fn drop(&mut self) {
        self.control_sender.close();
        if let Some(decoder_thread) = self.demuxer_thread.take() {
            decoder_thread.join().unwrap();
        }
    }
}
