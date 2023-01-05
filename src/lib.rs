use std::{
    collections::HashMap,
    io::{BufReader, BufWriter},
    sync::Arc,
};

use anyhow::{anyhow, Context as AnyCtx, Result};
use ashfire::target::Target;
use serenity::{
    async_trait,
    framework::standard::{
        macros::{command, group, hook},
        Args, CommandResult, StandardFramework,
    },
    model::prelude::*,
    prelude::*,
};
use shuttle_secrets::SecretStore;
use tracing::{error, info};
use wasmtime::*;
use wasmtime_wasi::sync::WasiCtxBuilder;

struct BotData;

impl TypeMapKey for BotData {
    type Value = Arc<RwLock<HashMap<MessageId, MessageId>>>;
}

struct Bot;

#[async_trait]
impl EventHandler for Bot {
    async fn message_update(&self, ctx: Context, msg: MessageUpdateEvent) {
        let lock = {
            let data_read = ctx.data.read().await;
            let data_lock = data_read
                .get::<BotData>()
                .expect("Expected BotData in TypeMap.");
            let hashmap = data_lock.read().await;
            hashmap.get(&msg.id).copied()
        };

        if let Some(id) = lock {
            if let Err(why) = edit(&ctx, msg, id).await {
                error!("Error in edit: {:?}", why);
            }
        }
    }

    async fn ready(&self, _: Context, ready: Ready) {
        info!("{} is connected!", ready.user.name);
    }
}

#[hook]
async fn after_hook(_: &Context, _: &Message, cmd_name: &str, error: CommandResult) {
    //  Print out an error if it happened
    if let Err(why) = error {
        error!("Error in {}: {:?}", cmd_name, why);
    }
}

#[shuttle_service::main]
async fn serenity(
    #[shuttle_secrets::Secrets] secret_store: SecretStore,
) -> shuttle_service::ShuttleSerenity {
    // Get the discord token set in `Secrets.toml`
    let Some(token) = secret_store.get("DISCORD_TOKEN") else {
        return Err(anyhow!("'DISCORD_TOKEN' was not found").into());
    };

    // Set gateway intents, which decides what events the bot will be notified about
    let intents = GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT;

    let framework = StandardFramework::new()
        .configure(|c| c.with_whitespace(false).prefix("?"))
        .group(&DEFAULT_GROUP)
        .after(after_hook);

    let client = Client::builder(&token, intents)
        .event_handler(Bot)
        .framework(framework)
        .type_map_insert::<BotData>(Arc::new(RwLock::new(HashMap::default())))
        .await
        .expect("Err creating client");

    Ok(client)
}

#[group("default")]
#[commands(eval)]
struct Default;

#[command]
async fn eval(ctx: &Context, msg: &Message, mut _args: Args) -> CommandResult {
    info!("Evaluating message: {}", msg.id);

    let output = compile_otput(&msg.content, &msg.author.name);
    let reply = msg.reply(ctx, output).await?;

    {
        let data_read = ctx.data.read().await;
        let data_lock = data_read
            .get::<BotData>()
            .expect("Expected BotData in TypeMap.");
        let mut hashmap = data_lock.write().await;
        hashmap.insert(msg.id, reply.id);
    };

    Ok(())
}

async fn edit(ctx: &Context, msg: MessageUpdateEvent, id: MessageId) -> CommandResult {
    info!("Evaluating edited message: {}", id);

    let name = &msg.author.map_or_else(String::new, |user| user.name);
    let message = &msg
        .content
        .with_context(|| "Failed to get the msg content")?;

    let output = compile_otput(message, name);

    msg.channel_id
        .edit_message(ctx, id, |m| m.content(output))
        .await?;
    Ok(())
}

fn compile_otput(message: &str, name: &str) -> String {
    match compile(message, name) {
        Ok(ok) => format!("Compilation result:\n```\n{ok}\n```"),
        Err(err) => format!("Compilation error:\n```\n{err}\n```"),
    }
}

fn compile(msg: &str, name: &str) -> Result<String> {
    let trimmed = msg
        .strip_prefix("?eval")
        .map(|s| s.trim_start())
        .and_then(|s| s.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .with_context(|| "Failed to parse a code block")?;

    let reader = &mut BufReader::new(trimmed.as_bytes());
    let mut writer = BufWriter::new(vec![]);

    ashfire::compile_buffer(name, reader, &mut writer, Target::Wasi, true)?;

    let output = writer.into_inner()?;
    run(&output)
}

fn run(wat: &[u8]) -> Result<String> {
    let engine = Engine::default();
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::add_to_linker(&mut linker, |s| s)?;

    let writer = wasi_common::pipe::WritePipe::new_in_memory();
    let wasi = WasiCtxBuilder::new()
        .stdout(Box::new(writer.clone()))
        .build();

    {
        let mut store = Store::new(&engine, wasi);
        let module = Module::new(&engine, wat)?;

        linker.module(&mut store, "", &module)?;
        linker
            .get_default(&mut store, "")?
            .typed::<(), ()>(&store)?
            .call(&mut store, ())?;
    }

    let vec = writer
        .try_into_inner()
        .expect("sole remaining reference to WritePipe")
        .into_inner();

    let output = String::from_utf8_lossy(&vec).to_string();
    Ok(output)
}
