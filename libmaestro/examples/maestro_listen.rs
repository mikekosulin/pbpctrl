//! Simple example for listening to Maestro messages sent via the RFCOMM channel.
//!
//! Usage:
//!   cargo run --example maestro_listen -- <bluetooth-device-address>

use std::str::FromStr;

use bluer::{Address, Session, Device};
use bluer::rfcomm::{Profile, ReqError, Role, ProfileHandle};

use futures::{StreamExt, Sink};

use maestro::protocol::codec::Codec;
use maestro::protocol::types::{SoftwareInfo, SettingsRsp};
use maestro::pwrpc::client::{Client, Request, Streaming};
use maestro::pwrpc::id::Identifier;
use maestro::pwrpc::types::RpcPacket;
use maestro::pwrpc::Error;


#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), anyhow::Error> {
    env_logger::init_from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "debug")
    );

    // handle command line arguments
    let addr = std::env::args().nth(1).expect("need device address as argument");
    let addr = Address::from_str(&addr)?;

    // set up session
    let session = Session::new().await?;
    let adapter = session.default_adapter().await?;

    println!("Using adapter '{}'", adapter.name());

    // get device
    let dev = adapter.device(addr)?;
    let uuids = {
        let mut uuids = Vec::from_iter(dev.uuids().await?
            .unwrap_or_default()
            .into_iter());

        uuids.sort_unstable();
        uuids
    };

    println!("Found device:");
    println!("  alias:     {}", dev.alias().await?);
    println!("  address:   {}", dev.address());
    println!("  paired:    {}", dev.is_paired().await?);
    println!("  connected: {}", dev.is_connected().await?);
    println!("  UUIDs:");
    for uuid in uuids {
        println!("    {}", uuid);
    }
    println!();

    // try to reconnect if connection is reset
    let stream = {
        // register GFPS profile
        println!("Registering Maestro profile...");

        let profile = Profile {
            uuid: maestro::UUID,
            role: Some(Role::Client),
            require_authentication: Some(false),
            require_authorization: Some(false),
            auto_connect: Some(false),
            ..Default::default()
        };

        let mut profile_handle = session.register_profile(profile).await?;

        // connect profile
        println!("Connecting GFPS profile...");
        connect_device_to_profile(&mut profile_handle, &dev).await?
    };

    println!("Profile connected");

    // set up stream for RPC communication
    let codec = Codec::new();
    let mut stream = codec.wrap(stream);

    // retreive the channel numer
    //
    // Note: this is a bit hacky. The protocol works with different channels,
    // depending on which bud is active (or case...), and which peer we
    // represent (Maestro A or B). Only one is responsive and ther doesn't seem
    // to be a good way to figure out which.
    //
    // The app seems to do this by firing off one GetSoftwareInfo request per
    // potential channel, waiting for responses and choosing the responsive
    // one. However, the buds also automatically send one GetSoftwareInfo
    // response on the right channel without a request right after establishing
    // a connection. So for now we just listen for that first message,
    // discarding all but the channel id.

    let mut channel = 0;

    while let Some(packet) = stream.next().await {
        match packet {
            Ok(packet) => {
                channel = packet.channel_id;
                break;
            }
            Err(e) => {
                Err(e)?
            }
        }
    }

    // set up RPC client
    let client = Client::new(stream);
    let handle = client.handle();

    tokio::spawn(run_client(client));

    println!("Sending GetSoftwareInfo request");
    println!();

    let req = Request {
        channel_id: channel,
        service_id: Identifier::new("maestro_pw.Maestro").hash(),
        method_id: Identifier::new("GetSoftwareInfo").hash(),
        call_id: 42,
        message: (),
    };

    let info: SoftwareInfo = handle.unary(req).await?
        .result().await?;

    println!("{:#?}", info);

    println!();
    println!("Listening to settings changes...");
    println!();

    let req = Request {
        channel_id: channel,
        service_id: Identifier::new("maestro_pw.Maestro").hash(),
        method_id: Identifier::new("SubscribeToSettingsChanges").hash(),
        call_id: 42,
        message: (),
    };

    let mut call: Streaming<SettingsRsp> = handle.server_streaming(req).await?;
    while let Some(msg) = call.stream().next().await {
        println!("{:#?}", msg?);
    }

    Ok(())
}

async fn run_client<S, E>(mut client: Client<S>)
where
    S: Sink<RpcPacket>,
    S: futures::Stream<Item = Result<RpcPacket, E>> + Unpin,
    Error: From<E>,
    Error: From<S::Error>,
{
    let result = client.run().await;

    if let Err(e) = result {
        log::error!("client shut down with error: {e:?}")
    }
}

async fn connect_device_to_profile(profile: &mut ProfileHandle, dev: &Device)
    -> bluer::Result<bluer::rfcomm::Stream>
{
    loop {
        tokio::select! {
            res = async {
                let _ = dev.connect().await;
                dev.connect_profile(&maestro::UUID).await
            } => {
                if let Err(err) = res {
                    println!("Connecting GFPS profile failed: {:?}", err);
                }
                tokio::time::sleep(std::time::Duration::from_millis(3000)).await;
            },
            req = profile.next() => {
                let req = req.expect("no connection request received");

                if req.device() == dev.address() {
                    println!("Accepting request...");
                    break req.accept();
                } else {
                    println!("Rejecting unknown device {}", req.device());
                    req.reject(ReqError::Rejected);
                }
            },
        }
    }
}
