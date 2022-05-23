//! Join a BLE mesh

// use uuid::Uuid;
use bluer::mesh::{application::Application, Element, Model};
use clap::Parser;
use std::io;
use std::io::prelude::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long)]
    token: String,
}

/// Temp function to examine the program
fn pause() {
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    write!(stdout, "Press any key to continue...").unwrap();
    stdout.flush().unwrap();

    let _ = stdin.read(&mut [0u8]).unwrap();
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();
    let session = bluer::Session::new().await?;

    let mesh = session.mesh().await?;

    let app = Application {
        path: "/example".to_string(),
        elements: vec![
            Element {
                models: vec![
                    Model {
                        id: 0x1000,
                        vendor: 0xffff, //None
                    },
                    Model {
                        id: 0x1100,
                        vendor: 0xffff, //None
                    },
                    Model {
                        id: 0x0001,
                        vendor: 0x05F1, // Linux Foundation Company ID
                    },
                ],
            },
            Element {
                models: vec![
                    Model {
                        id: 0x1001,
                        vendor: 0xffff, //None
                    },
                    Model {
                        id: 0x1102,
                        vendor: 0xffff, //None
                    },
                ],
            },
        ],
    };

    let _app = mesh.application(app).await?;

    mesh.print_dbus_objects().await?;

    //mesh.join("/example", Uuid::new_v4()).await?;

    mesh.attach("/example", &args.token).await?;

    //mesh.cancel().await?;

    //mesh.leave(token).await?;

    pause();

    Ok(())
}
