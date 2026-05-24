mod fuse;
pub mod layers;
use clap::Parser;
use fuser::MountOption;
use log::info;

#[derive(Parser)]
#[command(name = "mantle")]
#[command(about = "Mantle: A fault-tolerant, hybrid overlay file system", long_about = None)]
struct Cli {
    /// Source directory (backend)
    #[arg(short, long)]
    source: String,

    /// Mount point
    #[arg(short, long)]
    mountpoint: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    // Create the mountpoint directory if it does not exist
    if let Err(e) = std::fs::create_dir_all(&cli.mountpoint) {
        anyhow::bail!(
            "Could not create mountpoint directory {}: {}",
            cli.mountpoint,
            e
        );
    }

    info!("Mounting Mantle from {} to {}", cli.source, cli.mountpoint);

    let options = vec![MountOption::FSName("mantle".to_string())];

    let layer_m = std::sync::Arc::new(parking_lot::RwLock::new(layers::LayerM::new(&cli.source)?));
    layers::LayerM::start_background_fetch(layer_m.clone());

    let overlay = std::sync::Arc::new(layers::MantleOverlay::new());

    let fs = fuse::MantleFS::new(layer_m.clone(), overlay.clone());

    let _session = fuser::spawn_mount2(fs, &cli.mountpoint, &options)?;

    tokio::signal::ctrl_c().await?;
    info!("Ctrl-C received, unmounting and exiting...");

    Ok(())
}
