#![forbid(unsafe_code)]

use ncat_media::Book;
use ncat_protocol::{Envelope, ErrorCode, Page, Request, Response, read_message, write_message};
use std::io::{self, BufReader, BufWriter, Write};

fn pages(writer: &mut impl Write, id: u64, book: &Book) -> io::Result<()> {
    let mut chunk: Vec<Page> = Vec::new();
    let mut cost = 0;
    for page in &book.pages {
        if !chunk.is_empty()
            && (cost + page.cost() > ncat_protocol::PAGE_BATCH_BYTES
                || chunk.len() >= ncat_protocol::PAGE_BATCH_ENTRIES)
        {
            write_message(
                writer,
                &Envelope::new(
                    id,
                    Response::Pages {
                        pages: std::mem::take(&mut chunk),
                        selected: book.selected,
                        done: false,
                    },
                ),
            )?;
            cost = 0;
        }
        cost += page.cost();
        chunk.push(page.clone());
    }
    write_message(
        writer,
        &Envelope::new(
            id,
            Response::Pages {
                pages: chunk,
                selected: book.selected,
                done: true,
            },
        ),
    )
}

fn run() -> io::Result<()> {
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = BufWriter::new(io::stdout().lock());
    let mut book: Option<Book> = None;
    while let Some(message) = read_message::<Request>(&mut input)? {
        let result = match message.body {
            Request::Open { source } => {
                book = None;
                match Book::open(&source.path()) {
                    Ok(next) => {
                        write_message(
                            &mut output,
                            &Envelope::new(
                                message.id,
                                Response::Opened {
                                    title: next.title.clone(),
                                    deferred: next.deferred,
                                },
                            ),
                        )?;
                        pages(&mut output, message.id, &next)?;
                        book = Some(next);
                        Ok(None)
                    }
                    Err(error) => Err(error),
                }
            }
            Request::Catalog => match book.as_mut() {
                Some(book) => match book.catalog() {
                    Ok(()) => {
                        pages(&mut output, message.id, book)?;
                        Ok(None)
                    }
                    Err(e) => Err(e),
                },
                None => Err(ErrorCode::Protocol),
            },
            Request::Decode {
                key,
                output,
                max_width,
                max_height,
                workspace,
                quarter_turns,
            } => book
                .as_ref()
                .ok_or(ErrorCode::Protocol)
                .and_then(|book| {
                    book.decode(
                        &key,
                        &output.path(),
                        max_width,
                        max_height,
                        workspace,
                        quarter_turns,
                    )
                })
                .map(|frame| Some(Response::Frame(frame))),
            Request::Shutdown => {
                write_message(&mut output, &Envelope::new(message.id, Response::Bye))?;
                break;
            }
        };
        if let Some(response) = match result {
            Ok(response) => response,
            Err(e) => Some(Response::Error(e)),
        } {
            write_message(&mut output, &Envelope::new(message.id, response))?;
        }
    }
    Ok(())
}

// Resource containment, not a filesystem/network sandbox. Never raise inherited limits.
#[cfg(unix)]
fn protect() -> Result<(), rustix::io::Errno> {
    use rustix::process::{self, Pid, Resource, Rlimit, Signal};
    let parent = process::getppid()
        .filter(|p| *p != Pid::INIT)
        .ok_or(rustix::io::Errno::PERM)?;
    let inherited = process::getrlimit(Resource::As);
    let cap = ncat_protocol::WORKER_ADDRESS_SPACE;
    let maximum = inherited.maximum.unwrap_or(cap).min(cap);
    process::setrlimit(
        Resource::As,
        Rlimit {
            current: Some(inherited.current.unwrap_or(maximum).min(maximum)),
            maximum: Some(maximum),
        },
    )?;
    process::setrlimit(
        Resource::Core,
        Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    )?;
    process::set_parent_process_death_signal(Some(Signal::KILL))?;
    rustix::thread::set_no_new_privs(true)?;
    if process::getppid() != Some(parent) {
        return Err(rustix::io::Errno::PERM);
    }
    Ok(())
}

// On Windows the coordinator places this process in a job object right after spawning it
// (memory cap, no child processes, killed with the GUI) before sending any request; see
// `ncat_windows::WorkerJob`. A process cannot safely impose that on itself here.
#[cfg(windows)]
fn protect() -> Result<(), ()> {
    Ok(())
}

fn main() {
    let protected = protect().is_ok();
    std::panic::set_hook(Box::new(|_| eprintln!("ncat-worker: internal failure")));
    if !protected {
        eprintln!("ncat-worker: resource protection unavailable");
        std::process::exit(2);
    }
    if run().is_err() {
        eprintln!("ncat-worker: protocol failure");
        std::process::exit(1);
    }
}
