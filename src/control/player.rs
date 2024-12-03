extern crate ffmpeg_next as ffmpeg;

use std::path::PathBuf;

use futures::{future::OptionFuture, FutureExt};

use super::{audio, video};


#[derive(Clone, Copy, Debug)]
pub enum ControlCommand {
    Play,
    Pause,
}

pub struct Player {
    control_sender: smol::channel::Sender<ControlCommand>,
    demuxer_thread: Option<std::thread::JoinHandle<()>>,
    playing: bool,
    playing_changed_callback: Box<dyn Fn(bool)>,
}

impl Player {
    pub fn start(
        path: PathBuf,
        video_frame_callback: impl FnMut(&ffmpeg::util::frame::Video) + Send + 'static,
        playing_changed_callback: impl Fn(bool) + 'static,
    ) -> Result<Self, anyhow::Error> {
        println!("开始播放视频文件: {:?}", path);
        let (control_sender, control_receiver) = smol::channel::unbounded();

        let demuxer_thread =
            std::thread::Builder::new().name("demuxer thread".into()).spawn(move || {
                smol::block_on(async move {
                    println!("初始化输入上下文");
                    let mut input_context = ffmpeg::format::input(&path).unwrap();

                    println!("查找最佳视频流");
                    let video_stream =
                        input_context.streams().best(ffmpeg::media::Type::Video).unwrap();
                    let video_stream_index = video_stream.index();
                    println!("视频流索引: {}", video_stream_index);
                    let video_playback_thread = video::VideoPlaybackThread::start(
                        &video_stream,
                        Box::new(video_frame_callback),
                    )
                    .unwrap();

                    println!("查找最佳音频流");
                    let audio_stream =
                        input_context.streams().best(ffmpeg::media::Type::Audio).unwrap();
                    let audio_stream_index = audio_stream.index();
                    println!("音频流索引: {}", audio_stream_index);
                    let audio_playback_thread =
                        audio::AudioPlaybackThread::start(&audio_stream).unwrap();

                    let mut playing = true;

                    let packet_forwarder_impl = async {
                        println!("开始转发数据包");
                        for (stream, packet) in input_context.packets() {
                            if stream.index() == audio_stream_index {
                                println!("转发音频包");
                                audio_playback_thread.receive_packet(packet).await;
                            } else if stream.index() == video_stream_index {
                                println!("转发视频包");
                                video_playback_thread.receive_packet(packet).await;
                            }
                        }
                        println!("数据包转发完成");
                    }
                    .fuse()
                    .shared();

                    loop {
                        let packet_forwarder: OptionFuture<_> =
                            if playing { Some(packet_forwarder_impl.clone()) } else { None }.into();

                        smol::pin!(packet_forwarder);

                        futures::select! {
                            _ = packet_forwarder => {
                                println!("播放器播放完成");
                            },
                            received_command = control_receiver.recv().fuse() => {
                                match received_command {
                                    Ok(command) => {
                                        println!("收到控制命令: {:?}", command);
                                        video_playback_thread.send_control_message(command).await;
                                        audio_playback_thread.send_control_message(command).await;
                                        match command {
                                            ControlCommand::Play => {
                                                println!("继续播放");
                                                playing = true;
                                            },
                                            ControlCommand::Pause => {
                                                println!("暂停播放");
                                                playing = false;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        println!("播放器控制通道关闭: {}", e);
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

    pub fn toggle_pause_playing(&mut self) {
        if self.playing {
            println!("切换到暂停状态");
            self.playing = false;
            self.control_sender.send_blocking(ControlCommand::Pause).unwrap();
        } else {
            println!("切换到播放状态");
            self.playing = true;
            self.control_sender.send_blocking(ControlCommand::Play).unwrap();
        }
        (self.playing_changed_callback)(self.playing);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        println!("Player dropped");
        self.control_sender.close();
        if let Some(decoder_thread) = self.demuxer_thread.take() {
            println!("等待解码线程结束");
            decoder_thread.join().unwrap();
        }
    }
}