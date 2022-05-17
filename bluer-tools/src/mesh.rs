//use uuid::Uuid;
use bluer::mesh::{
    application::Application,
    Element, Model,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::init();
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
                    }
                ]
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
                    Model {
                        id: 0x0001,
                        vendor: 0x05F1, // Linux Foundation Company ID
                    }
                ]
            }
        ],
    };


    //let _app = mesh.application("/example").await?;
    let _app = mesh.application(app).await?;

    mesh.print_dbus_objects().await?;

    // mesh.join("/example", Uuid::new_v4()).await?;


    let token = "26ea5cc2f46fd59d";

    mesh.attach("/example", token).await?;

    //mesh.cancel().await?;

    //mesh.leave(token).await?;

    Ok(())
}
