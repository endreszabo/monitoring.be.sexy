use std::io;
use std::io::{Read, Write, copy};
use vsock::{SockAddr, VsockAddr, VsockStream, VMADDR_CID_HOST, VMADDR_CID_LOCAL};
use std::fs;
use tracing_subscriber::FmtSubscriber;
use tracing_subscriber::fmt;
use command_fds::{CommandFdExt, FdMapping};
use std::panic;
use std::process::{Command, Stdio};
use std::io::{BufRead, BufReader};
use std::sync::mpsc::{SyncSender, Receiver, sync_channel};
use std::str::from_utf8;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::FromRawFd;

use os_pipe::pipe;

use tracing::{debug, error, info, span, warn, Level};

fn main() {
    let mut stream =
        VsockStream::connect(&SockAddr::Vsock(VsockAddr::new(VMADDR_CID_HOST, 8000))).expect("connection to MicroVM controller VSock failed");

    let format = tracing_subscriber::fmt::format().pretty();
    fmt().with_max_level(Level::DEBUG).event_format(format).init();
    panic::set_hook(Box::new(|_info| {
        error!("{}",_info);
    }));
    let mut microvm_id: i32 = -1;

    let mut data = fs::read_to_string("/proc/cmdline").expect("Unable to kernel commandline file");
    data.push_str("mcbs_microvm_id=2000");
    for clause in data.split_whitespace() {
        println!("'{}'", clause);
        if clause.contains('=') {
            let kv: Vec<&str> = clause.split('=').collect();
            match kv[0] {
                "mcbs_microvm_id" => {
                    println!("ID: {}", kv[1]);
                    match kv[1].parse::<i32>() {
                        Ok(id) => {
                            microvm_id = id;
                            debug!(microvm_id = id, "MicroVM ID set")
                        },
                        Err(e) => panic!("Could not parse the microvm_id; error='{}', microvm_id='{}", e, kv[1]),
                    }
                },
                _ => {
                    debug!("Unprocessed kernel commandline KV-item: {}", kv[0]);
                }
            }
        } else {
            debug!("Unprocessed kernel commandline clause: {}", clause);
        }
    }
    if microvm_id == -1 {
        panic!("MicroVM ID is undefined");
    }
    info!(microvm_id = microvm_id, "Started up");
    info!("IP address: 44.128.{}.{}", microvm_id / 256,microvm_id % 256);
    info!("IPv6 address: fe80::{:02x}{:02x}", microvm_id / 256,microvm_id % 256);
    const STDIO_BUFLEN: usize = 64;

    enum StdioType {
        Stdout,
        Stderr,
        Metrics,
        Debug
    }
    struct StdioMsg {
        stdio_type: StdioType,
        read_size: usize,
        buf: [u8; STDIO_BUFLEN]
    }
    enum MsgType {
        Stdout{ size_read: usize, buf: [u8; STDIO_BUFLEN] },
        Stderr{ size_read: usize, buf: [u8; STDIO_BUFLEN] },
        Metric{ size_read: usize, buf: [u8; STDIO_BUFLEN] },
        Debug{ msg: String }
    }

    let (tx, rx): (SyncSender<MsgType>, Receiver<MsgType>) = sync_channel(3);

    let (mut pipe_reader, pipe_writer) = pipe().unwrap();

    let mut child_shell = Command::new("fdfiddle.sh");
    child_shell.stdin(Stdio::piped());
    child_shell.stdout(Stdio::piped());
    child_shell.stderr(Stdio::piped());
    child_shell.fd_mappings(vec![
        FdMapping {
            parent_fd: pipe_writer.as_raw_fd(),//MetricsPipe.as_raw_fd(),
            child_fd: 3
        }
    ]).unwrap();
    let mut child = child_shell.spawn().unwrap();


//    let child_in = child_shell.stdin.as_mut().unwrap();
    //let mut child_out = BufReader::new(child_shell.stdout.as_mut().unwrap());
    //let mut line = String::new();

    let mut command_stdin = child.stdin.take().unwrap();
    println!("command_stdin {:?}", command_stdin);

    let copy_stdin_thread = std::thread::spawn(move || {
        io::copy(&mut io::stdin(), &mut command_stdin)
    });
    copy_stdin_thread.join().unwrap();
    let mut command_stdout = child.stdout.take().unwrap();
    {
        let tx=tx.clone();
        let copy_stdout_thread = std::thread::spawn(move || {
            let mut buffer = [0; 64];
            loop {
                match command_stdout.read(&mut buffer[..]) {
                    Ok(bytes) => {
                        if bytes>0 {
                            match tx.send(MsgType::Stdout { size_read: bytes, buf: buffer }) {
                                Ok(_) => {print!("stdout message sent")},
                                Err(e) => {
                                    panic!("error at sending: {:?}", e);
                                }
                            }
                        } else {
                            debug!("Stdout socket closed");
                            break;
                        }
                    }
                    Err(e) => {
                        panic!("Error: {:?}", e);
                    }
                }
            }
            println!("dropping tx of stdout");
            drop(tx);
        });
//      copy_stdout_thread.join().unwrap();
    }

    let mut command_stderr = child.stderr.take().unwrap();
    {
        let tx=tx.clone();
        let copy_stderr_thread = std::thread::spawn(move || {
            let mut buffer = [0; 64];
            loop {
                match command_stderr.read(&mut buffer[..]) {
                    Ok(bytes) => {
                        if bytes>0 {
                            match tx.send(MsgType::Stderr { size_read: bytes, buf: buffer }) {
                                Ok(_) => {print!("stderr message sent")},
                                Err(e) => {
                                    panic!("error at sending: {:?}", e);
                                }
                            }
                        } else {
                            debug!("Stderr socket closed");
                            break;
                        }
                    }
                    Err(e) => {
                        panic!("Error: {:?}", e);
                    }
                }
            }
            println!("dropping tx of stderr");
            drop(tx);
        });
//        copy_stderr_thread.join().unwrap();
    }


// metrics collection disabled until we can detect socket closing 
//    {
//        let tx=tx.clone();
//        let copy_metrics_thread = std::thread::spawn(move || {
//            let mut buffer = [0; 64];
//            loop {
//                match pipe_reader.read(&mut buffer[..]) {
//                    Ok(bytes) => {
//                        if bytes>0 {
//                            match tx.send(MsgType::Metric { size_read: bytes, buf: buffer }) {
//                                Ok(_) => {print!("metrics message sent")},
//                                Err(e) => {
//                                    panic!("error at sending: {:?}", e);
//                                }
//                            }
//                        } else {
//                            debug!("Metrics socket closed");
//                            break;
//                        }
//                    }
//                    Err(e) => {
//                        panic!("Error: {:?}", e);
//                    }
//                }
//            }
//            println!("dropping tx of metrics");
//            drop(tx);
//        });
////        copy_metrics_thread.join().unwrap();
//    }
    println!("dropping last tx");
    drop(tx);
    while let Ok(msg) = rx.recv() {
        match msg {
            MsgType::Stdout{size_read, buf} => {
                println!("{:?}",&buf[..size_read]);
                stream.write(&buf[..size_read]).expect("can't send via vsock");
                println!("Got stdout buffer: {}, {:?}, string: '{}'", size_read, buf, from_utf8(&buf[..size_read]).unwrap());
            },
            MsgType::Stderr{size_read, buf} => {
                println!("Got stderr buffer: {}, {:?}, string: '{}'", size_read, buf, from_utf8(&buf[..size_read]).unwrap());
            },
            MsgType::Metric{size_read, buf} => {
                println!("Got metrics buffer: {}, {:?}, string: '{}'", size_read, buf, from_utf8(&buf[..size_read]).unwrap());
            },
            MsgType::Debug{msg} => {
                println!("Got debug buffer: {:?}", msg);
            }
        }
    }
    match child.wait() {
        Ok(ir) => {
            panic!("Child ended OK: {:?}", ir);
        }
        Err(e) => {
            panic!("Child error: {:?}", e);
        }
    }

//    loop {
        //child_in.write("ls\n".as_bytes()).unwrap();
        //println!(child_out.read_to_string(line));
        //child_out.read(&mut line).unwrap();
        //println!("{}", line);
    //}

//    while tx_pos < TEST_BLOB_SIZE {
//        let written_bytes = stream
//            .write(&blob[tx_pos..tx_pos + TEST_BLOCK_SIZE])
//            .expect("write failed");
//        if written_bytes == 0 {
//            panic!("stream unexpectedly closed");
//        }
//
//        let mut rx_pos = tx_pos;
//        while rx_pos < (tx_pos + written_bytes) {
//            let read_bytes = stream.read(&mut rx_blob[rx_pos..]).expect("read failed");
//            if read_bytes == 0 {
//                panic!("stream unexpectedly closed");
//            }
//            rx_pos += read_bytes;
//        }
//
//        tx_pos += written_bytes;
//    }
//
}

