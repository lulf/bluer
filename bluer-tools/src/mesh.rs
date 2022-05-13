use tokio::time::{sleep, Duration};
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::init();
    let session = bluer::Session::new().await?;

    let mesh = session.mesh().await?;

    let _app = mesh.application("/example").await?;

    mesh.print_dbus_objects().await?;

    // mesh.join("/example", Uuid::new_v4()).await?;


    let token = "26ea5cc2f46fd59d";

    mesh.attach("/example", token).await?;

    //mesh.cancel().await?;

    //mesh.leave(token).await?;

    Ok(())
}
