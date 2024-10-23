// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Context, Result};
use calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction};
use sendfd::{RecvWithFd, SendWithFd};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap, env, io::{ErrorKind, Read, Write}, os::unix::{
        io::{AsFd, BorrowedFd, FromRawFd, RawFd},
        net::UnixStream,
    }, path::PathBuf
};
use tracing::{error, warn, info};

use crate::State;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "message")]
pub enum Message {
    SetEnv { variables: HashMap<String, String> },
    NewPrivilegedClient { count: usize },
}

struct StreamWrapper {
    stream: UnixStream,
    buffer: Vec<u8>,
    size: u16,
    read_bytes: usize,
}
impl AsFd for StreamWrapper {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}
impl From<UnixStream> for StreamWrapper {
    fn from(stream: UnixStream) -> StreamWrapper {
        StreamWrapper {
            stream,
            buffer: Vec::new(),
            size: 0,
            read_bytes: 0,
        }
    }
}

unsafe fn set_cloexec(fd: RawFd) -> rustix::io::Result<()> {
    if fd == -1 {
        return Err(rustix::io::Errno::BADF);
    }
    let fd = BorrowedFd::borrow_raw(fd);
    let flags = rustix::io::fcntl_getfd(fd)?;
    rustix::io::fcntl_setfd(fd, flags | rustix::io::FdFlags::CLOEXEC)
}

pub fn get_env() -> Result<HashMap<String, String>> {
    let mut env = HashMap::new();
    env.insert(
        String::from("WAYLAND_DISPLAY"),
        env::var("WAYLAND_DISPLAY").expect("No WAYLAND_DISPLAY"),
    );
    if let Ok(var) = env::var("DISPLAY") {
        env.insert(String::from("DISPLAY"), var);
    }
    if let Ok(var) = env::var("SWAYSOCK") {
        env.insert(String::from("SWAYSOCK"), var);
    }
    if let Ok(var) = env::var("NIRI_SOCKET") {
        env.insert(String::from("NIRI_SOCKET"), var);
    }
    Ok(env)
}

pub fn setup_socket(handle: LoopHandle<State>) -> Result<()> {
    let fd = std::env::var("COSMIC_SESSION_SOCK")
        .context("Failed to find cosmic session socket")?
        .parse::<RawFd>()
        .context("COSMIC_SESSION_SOCK is no valid file descriptor")?;

    let mut session_socket = match unsafe { set_cloexec(fd) } {
        // CLOEXEC worked and we can startup with session IPC
        Ok(_) => unsafe { UnixStream::from_raw_fd(fd) },
        // CLOEXEC didn't work, something is wrong with the fd, just close it
        Err(err) => {
            unsafe { rustix::io::close(fd) };
            return Err(err).with_context(|| "Failed to setup session socket");
        }
    };

    let env = get_env()?;
    let message = serde_json::to_string(&Message::SetEnv { variables: env })
        .with_context(|| "Failed to encode environment variables into json")?;
    let bytes = message.into_bytes();
    let len = (bytes.len() as u16).to_ne_bytes();
    session_socket
        .write_all(&len)
        .with_context(|| "Failed to write message len")?;
    session_socket
        .write_all(&bytes)
        .with_context(|| "Failed to write message bytes")?;

    handle.insert_source(
        Generic::new(StreamWrapper::from(session_socket), Interest::READ, Mode::Level),
        move |_, stream, state| {
            // SAFETY: We don't drop the stream!
            let stream = unsafe { stream.get_mut() };

            if stream.size == 0 {
                let mut len = [0u8; 2];
                match stream.stream.read_exact(&mut len) {
                    Ok(()) => {
                        stream.size = u16::from_ne_bytes(len);
                        stream.buffer = vec![0; stream.size as usize];
                    },
                    Err(err) => {
                        warn!(?err, "Error reading from session socket");
                        return Ok(PostAction::Remove);
                    }
                }
            }

            stream.read_bytes += match stream.stream.read(&mut stream.buffer) {
                Ok(size) => size,
                Err(err) => {
                    error!(?err, "Error reading from session socket");
                    return Ok(PostAction::Remove);
                }
            };

            if stream.read_bytes != 0 && stream.read_bytes == stream.size as usize {
                stream.size = 0;
                stream.read_bytes = 0;
                match std::str::from_utf8(&stream.buffer) {
                    Ok(message) => {
                        match serde_json::from_str::<'_, Message>(&message) {
                            Ok(Message::NewPrivilegedClient { count }) => {
                                let mut buffer = [0; 1];
                                let mut fds = vec![0; count];
                                match stream.stream.recv_with_fd(&mut buffer, &mut *fds) {
                                    Ok((_, received_count)) => {
                                        assert_eq!(received_count, count);
                                        for fd in fds.into_iter().take(received_count) {
                                            if fd == -1 {
                                                continue;
                                            }
                                            let client_stream = unsafe { UnixStream::from_raw_fd(fd) };

                                            let Some(socket_name) = env::var_os("WAYLAND_DISPLAY")
                                                .map(Into::<PathBuf>::into) else { continue };

                                            let socket_path = if socket_name.is_absolute() {
                                                socket_name
                                            } else {
                                                let Some(mut socket_path) = env::var_os("XDG_RUNTIME_DIR").map(Into::<PathBuf>::into) else { continue };
                                                if !socket_path.is_absolute() {
                                                    continue;
                                                }
                                                socket_path.push(socket_name);
                                                socket_path
                                            };
                                            match UnixStream::connect(socket_path) {
                                                Ok(server_stream) => {
                                                    let client_stream_clone = match client_stream.try_clone() {
                                                        Ok(stream) => stream,
                                                        Err(err) => {
                                                            warn!(?err, "Failed to forward wayland connection");
                                                            continue;
                                                        },
                                                    };
                                                    let server_stream_clone = match server_stream.try_clone() {
                                                        Ok(stream) => stream,
                                                        Err(err) => {
                                                            warn!(?err, "Failed to forward wayland connection");
                                                            continue;
                                                        },
                                                    };

                                                    if let Err(err) = state.loop_handle.insert_source(Generic::new(server_stream, Interest::READ, Mode::Level), move |_, stream, _| {
                                                        let mut buf = [0u8; 1024];
                                                        let mut fds = [0i32; 4];
                                                        // SAFETY: We don't drop the stream
                                                        let stream = unsafe { stream.get_mut() };
                                                        match stream.recv_with_fd(&mut buf, &mut fds) {
                                                            Ok((bytes, fd_count)) if bytes > 0 || fd_count > 0 => {
                                                                let mut buf = &buf[0..bytes];
                                                                let mut fds = &fds[0..fd_count];
                                                                while !buf.is_empty() {
                                                                    match client_stream_clone.send_with_fd(buf, fds) {
                                                                        Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
                                                                        Ok(0) => {
                                                                            return Ok(PostAction::Remove);
                                                                        }
                                                                        Ok(n) => {
                                                                            buf = &buf[n..];
                                                                            fds = &fds[0..0];
                                                                        },
                                                                        Err(_) => return Ok(PostAction::Remove),
                                                                    }
                                                                }
                                                                Ok(PostAction::Continue)
                                                            }
                                                            Err(err) if err.kind() == ErrorKind::Interrupted => Ok(PostAction::Continue),
                                                            x => {
                                                                info!(?x, "client disconnected");
                                                                let _ = client_stream_clone.shutdown(std::net::Shutdown::Both);
                                                                let _ = stream.shutdown(std::net::Shutdown::Both);
                                                                Ok(PostAction::Remove)
                                                            }
                                                        }
                                                    }) {
                                                        warn!(?err, "Failed to forward wayland connection");
                                                    }
                                                    
                                                    if let Err(err) = state.loop_handle.insert_source(Generic::new(client_stream, Interest::READ, Mode::Level), move |_, stream, _| {
                                                        let mut buf = [0u8; 1024];
                                                        let mut fds = [0i32; 4];
                                                        // SAFETY: We don't drop the stream
                                                        let stream = unsafe { stream.get_mut() };
                                                        match stream.recv_with_fd(&mut buf, &mut fds) {
                                                            Ok((bytes, fd_count)) if bytes > 0 || fd_count > 0 => {
                                                                let mut buf = &buf[0..bytes];
                                                                let mut fds = &fds[0..fd_count];
                                                                while !buf.is_empty() {
                                                                    match server_stream_clone.send_with_fd(buf, fds) {
                                                                        Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
                                                                        Ok(0) => {
                                                                            return Ok(PostAction::Remove);
                                                                        }
                                                                        Ok(n) => {
                                                                            buf = &buf[n..];
                                                                            fds = &fds[0..0];
                                                                        },
                                                                        Err(_) => return Ok(PostAction::Remove),
                                                                    }
                                                                }
                                                                Ok(PostAction::Continue)
                                                            }
                                                            Err(err) if err.kind() == ErrorKind::Interrupted => Ok(PostAction::Continue),
                                                            x => {
                                                                info!(?x, "client disconnected");
                                                                let _ = stream.shutdown(std::net::Shutdown::Both);
                                                                let _ = server_stream_clone.shutdown(std::net::Shutdown::Both);
                                                                Ok(PostAction::Remove)
                                                            }
                                                        }
                                                    }) {
                                                        warn!(?err, "Failed to forward wayland connection");
                                                    }
                                                },
                                                Err(err) => {
                                                    warn!(?err, "Failed to connect to wayland socket");
                                                }
                                            } 
                                        }
                                    },
                                    Err(err) => {
                                        warn!(?err, "Failed to read file descriptors from session sock");
                                    }
                                }
                            },
                            Ok(Message::SetEnv { .. }) => warn!("Got SetEnv from session? What is this?"),
                            _ => warn!("Unknown session socket message, are you using incompatible cosmic-session and cosmic-comp versions?"),
                        };
                        Ok(PostAction::Continue)
                    },
                    Err(err) => {
                        warn!(?err, "Invalid message from session sock");
                        Ok(PostAction::Continue)
                    }
                }
            } else {
                Ok(PostAction::Continue)
            }
        },
    ).with_context(|| "Failed to init the cosmic session socket source")?;

    Ok(())
}
