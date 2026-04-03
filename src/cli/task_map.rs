/// Task map CLI tool
/// Generates task maps from Workflowy tags

use clap::Parser;
use workflowy_mcp_server::error::Result;

#[derive(Parser)]
#[command(name = "task-map")]
#[command(about = "Generate task maps from Workflowy tags", long_about = None)]
struct Args {
    /// Tag to query
    #[arg(value_name = "TAG")]
    tag: String,

    /// Output file path
    #[arg(short, long)]
    output: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    println!("Generating task map for tag: {}", args.tag);
    
    // TODO: Implement task map generation
    
    Ok(())
}
