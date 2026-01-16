use dotenvy::dotenv;
use std::env;

fn load_env() -> Result<String, String> {
    dotenv().ok();

    env::var("RPC_URL").map_err(|_| "RPC_URL not set".to_string())
}

fn main() {
    match load_env() {
        Ok(rpc_url) => {
            println!("Application started");
            println!("RPC_URL loaded: {}", rpc_url);
        }
        Err(err) => {
            eprintln!("Startup error: {}", err);
            std::process::exit(1);
        }
    }
}
// test
