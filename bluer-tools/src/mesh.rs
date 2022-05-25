#![feature(generic_associated_types)]
//! Join a BLE mesh

// use uuid::Uuid;
use bluer::mesh::{application::Application, *};
use clap::Parser;
use drogue_device::drivers::ble::mesh::model::{
    firmware::FirmwareUpdateClient,
    sensor::{PropertyId, SensorClient, SensorConfig, SensorData, SensorDescriptor},
};
use std::{io, io::prelude::*};

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
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args = Args::parse();
    let session = bluer::Session::new().await?;

    let mesh = session.mesh().await?;

    let app = Application {
        path: "/example".to_string(),
        elements: vec![
            Element { models: vec![Box::new(FromDrogue::new(SensorClient::<SensorModel, 1, 1>::new()))] },
            Element { models: vec![Box::new(FromDrogue::new(FirmwareUpdateClient))] },
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

#[derive(Clone, Debug)]
pub struct SensorModel;

#[derive(Clone, Debug)]
pub struct Temperature(i8);

impl SensorConfig for SensorModel {
    type Data<'m> = Temperature;

    const DESCRIPTORS: &'static [SensorDescriptor] = &[SensorDescriptor::new(PropertyId(0x4F), 1)];
}

impl SensorData for Temperature {
    fn decode(&mut self, id: PropertyId, params: &[u8]) -> Result<(), ParseError> {
        if id.0 == 0x4F {
            self.0 = params[0] as i8;
            Ok(())
        } else {
            Err(ParseError::InvalidValue)
        }
    }

    fn encode<const N: usize>(
        &self, _: PropertyId, xmit: &mut heapless::Vec<u8, N>,
    ) -> Result<(), InsufficientBuffer> {
        xmit.extend_from_slice(&self.0.to_le_bytes()).map_err(|_| InsufficientBuffer)?;
        Ok(())
    }
}
