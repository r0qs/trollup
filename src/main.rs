use std::{
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use clap::{Parser, Subcommand};
use ethers::{
    abi::Address,
    core::k256::SecretKey,
    providers::{Http, Provider},
    signers::{LocalWallet, Signer},
    types,
    utils::keccak256,
};
use hyper::Method;
use jsonrpsee::{
    server::{AllowHosts, ServerBuilder, ServerHandle},
    RpcModule,
};
use serde::{Deserialize, Serialize};
use tokio::{task, time::interval};
use tower_http::cors::{Any, CorsLayer};

mod node;

#[derive(Debug, Serialize, Deserialize)]
struct Tx {
    from: Address,
    to: Address,
    nonce: types::U256,
    value: types::U256,
}

impl From<CLITx> for Tx {
    fn from(tx: CLITx) -> Self {
        Self {
            from: tx.from,
            to: tx.to,
            nonce: tx.nonce,
            value: tx.value,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct SignedTx {
    tx: Tx,
    signature: String,
}

impl From<CLITx> for SignedTx {
    fn from(tx: CLITx) -> Self {
        Self {
            tx: Tx {
                from: tx.from,
                to: tx.to,
                nonce: tx.nonce,
                value: tx.value,
            },
            signature: tx.signature.unwrap(),
        }
    }
}

type Db = Arc<Mutex<Vec<SignedTx>>>;

const DB_PATH: &str = "./db";
const SOCKET_ADDRESS: &str = "127.0.0.1:38171";
const SERVER_ADDRESS: &str = "http://localhost:38171";

#[derive(Debug, Parser)]
#[clap(name = "trollup sequencer", version = env!("CARGO_PKG_VERSION"))]
struct Opts {
    #[clap(subcommand)]
    pub sub: Option<Subcommands>,
}

#[derive(Debug, Subcommand)]
pub enum Subcommands {
    #[clap(about = "Sign a trollup transaction.")]
    Sign(CLITx),
    #[clap(about = "Send trollup transaction, potentially sign it before.")]
    Send(CLITx),
}

#[derive(Debug, Clone, Parser, Default)]
pub struct CLITx {
    #[clap(
        long,
        short = 'p',
        value_name = "PRIVATE_KEY",
        help = "The private key that signs the message",
        default_value = "0x0000000000000000000000000000000000000000000000000000000000000000"
    )]
    pub private_key: ethers::types::H256,
    #[clap(
        long,
        short = 'f',
        value_name = "FROM_ADDRESS",
        help = "The address of the from address.",
        default_value = "0x0000000000000000000000000000000000000000"
    )]
    pub from: ethers::types::Address,
    #[clap(
        long,
        short = 't',
        value_name = "DEST_ADDRESS",
        help = "The address of the destination address.",
        default_value = "0x0000000000000000000000000000000000000000"
    )]
    pub to: ethers::types::Address,
    #[clap(
        long,
        short = 'v',
        value_name = "VALUE",
        help = "The value of the transaction.",
        default_value = "0"
    )]
    pub value: ethers::types::U256,
    #[clap(
        long,
        short = 'n',
        value_name = "NONCE",
        help = "The nonce of the transaction.",
        default_value = "0"
    )]
    pub nonce: ethers::types::U256,
    #[clap(
        long,
        short = 's',
        value_name = "SIGNATURE",
        help = "The signed transaction.",
        default_value = ""
    )]
    pub signature: Option<String>,
}

async fn run_node() -> anyhow::Result<()> {
    let db_path = Path::new(DB_PATH);
    let db = init_db(db_path);
    let rpc = init_rpc(db.clone()).await.unwrap();
    //let l1 = init_l1(db.clone());

    task::spawn(async move {
        let mut interval = interval(Duration::from_millis(1000 * 5));

        loop {
            interval.tick().await;
            let mut db = db.lock().unwrap();
            println!("submit transactions {:#?}", db);
            db.drain(..);
        }
    });

    tokio::spawn(rpc.stopped());

    println!("Run the following snippet in the developer console in any Website.");
    println!(
        r#"
        fetch("http://{}", {{
            method: 'POST',
            mode: 'cors',
            headers: {{ 'Content-Type': 'application/json' }},
            body: JSON.stringify({{
                jsonrpc: '2.0',
                method: 'submit_transaction',
                params: {{
                    from: '0x0000000000000000000000000000000000000000',
                    to: '0x0000000000000000000000000000000000000000',
                    amount: 42
                }},
                id: 1
            }})
        }}).then(res => {{
            console.log("Response:", res);
            return res.text()
        }}).then(body => {{
            console.log("Response Body:", body)
        }});
    "#,
        SOCKET_ADDRESS
    );

    futures::future::pending().await
}

fn hash_tx(sig_args: &Tx) -> ethers::types::TxHash {
    let mut value_bytes = vec![0; 32];
    sig_args.value.to_big_endian(&mut value_bytes);

    let mut nonce_bytes = vec![0; 32];
    sig_args.nonce.to_big_endian(&mut nonce_bytes);

    let msg = [
        sig_args.from.as_fixed_bytes().to_vec(),
        sig_args.to.as_fixed_bytes().to_vec(),
        value_bytes,
        nonce_bytes,
    ]
    .concat();

    types::TxHash::from(keccak256(msg))
}

async fn sign(sig_args: CLITx) -> anyhow::Result<types::Signature> {
    let wallet: LocalWallet = SecretKey::from_be_bytes(sig_args.private_key.as_bytes())
        .expect("invalid private key")
        .into();

    let hash = hash_tx(&sig_args.into()).as_fixed_bytes().to_vec();
    let signature = wallet.sign_message(hash.clone()).await?;

    Ok(signature)
}

fn verify_tx_signature(signed_tx: &SignedTx) -> anyhow::Result<()> {
    let hash = hash_tx(&signed_tx.tx).as_fixed_bytes().to_vec();
    let decoded = signed_tx.signature.parse::<types::Signature>()?;
    decoded.verify(hash, signed_tx.tx.from)?;

    Ok(())
}

async fn send(send_args: CLITx) -> anyhow::Result<()> {
    let signed: SignedTx = if send_args.signature.is_some() {
        send_args.into()
    } else {
        SignedTx {
            tx: send_args.clone().into(),
            signature: sign(send_args).await?.to_string(),
        }
    };

    verify_tx_signature(&signed)?;

    let provider =
        Provider::<Http>::try_from(SERVER_ADDRESS)?.interval(Duration::from_millis(10u64));
    let client = Arc::new(provider);
    let tx_receipt = client.request("submit_transaction", signed).await?;
    println!("{:?}", tx_receipt);

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = Opts::parse();
    match opts.sub {
        Some(Subcommands::Sign(sig_args)) => {
            let signature = sign(sig_args).await?;
            println!("{}", signature);
            Ok(())
        }
        Some(Subcommands::Send(send_args)) => send(send_args).await,
        _ => run_node().await,
    }
}

fn init_db(path: &Path) -> Db {
    Arc::new(Mutex::new(vec![]))
}

fn init_l1(db: Db) -> Provider<Http> {
    Provider::<Http>::try_from("https://mainnet.infura.io/v3/YOUR_API_KEY").unwrap()
}

async fn init_rpc(db: Db) -> anyhow::Result<ServerHandle> {
    let cors = CorsLayer::new()
        // Allow `POST` when accessing the resource
        .allow_methods([Method::POST])
        // Allow requests from any origin
        .allow_origin(Any)
        .allow_headers([hyper::header::CONTENT_TYPE]);
    let middleware = tower::ServiceBuilder::new().layer(cors);

    let server = ServerBuilder::default()
        .set_host_filtering(AllowHosts::Any)
        .set_middleware(middleware)
        .build(SOCKET_ADDRESS.parse::<SocketAddr>()?)
        .await?;

    println!("{}", server.local_addr().unwrap());

    let mut module = RpcModule::new(());
    module.register_method("submit_transaction", move |params, _| {
        println!("received transaction! {:?}", params);
        let tx: SignedTx = params.parse()?;

        verify_tx_signature(&tx)?;

        let mut db = db.lock().unwrap();
        db.push(tx);
        Ok(())
    })?;

    let handle = server.start(module)?;

    Ok(handle)
}
