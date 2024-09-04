use async_openai::{
    types::{
        CreateMessageRequestArgs, CreateRunRequestArgs, CreateThreadRequestArgs, MessageContent,
        RunStatus,
    },
    Client,
};
use flowsnet_platform_sdk::logger;
use tg_flows::{listen_to_update, update_handler, Telegram, UpdateKind};

#[no_mangle]
#[tokio::main(flavor = "current_thread")]
pub async fn on_deploy() {
    logger::init();

    // Start listening to Telegram updates
    let telegram_token = std::env::var("telegram_token").expect("Missing TELEGRAM_TOKEN");
    listen_to_update(telegram_token).await;
}

#[update_handler]
async fn handler(update: tg_flows::Update) {
    logger::init();
    let telegram_token = std::env::var("telegram_token").expect("Missing TELEGRAM_TOKEN");
    let tele = Telegram::new(telegram_token);

    if let UpdateKind::Message(msg) = update.kind {
        let text = msg.text().unwrap_or("");
        let chat_id = msg.chat.id;

        // Check if a thread ID exists for the chat, else create a new one
        let thread_id = match store_flows::get(chat_id.to_string().as_str()) {
            Some(ti) => {
                if text == "/restart" {
                    delete_thread(ti.as_str().unwrap()).await;
                    store_flows::del(chat_id.to_string().as_str());
                    return;
                }
                ti.as_str().unwrap().to_owned()
            }
            None => {
                let ti = create_thread().await;
                store_flows::set(
                    chat_id.to_string().as_str(),
                    serde_json::Value::String(ti.clone()),
                    None,
                );
                ti
            }
        };

        // Send the user's message to OpenAI and retrieve the response
        let response = run_message(thread_id.as_str(), String::from(text)).await;
        let _ = tele.send_message(chat_id, response).await;
    }
}

async fn create_thread() -> String {
    let client = Client::new();
    let create_thread_request = CreateThreadRequestArgs::default().build().unwrap();

    match client.threads().create(create_thread_request).await {
        Ok(to) => {
            log::info!("New thread (ID: {}) created.", to.id);
            to.id
        }
        Err(e) => {
            panic!("Failed to create thread. {:?}", e);
        }
    }
}

async fn delete_thread(thread_id: &str) {
    let client = Client::new();

    match client.threads().delete(thread_id).await {
        Ok(_) => {
            log::info!("Old thread (ID: {}) deleted.", thread_id);
        }
        Err(e) => {
            log::error!("Failed to delete thread. {:?}", e);
        }
    }
}

async fn run_message(thread_id: &str, text: String) -> String {
    let client = Client::new();
    let assistant_id = std::env::var("ASSISTANT_ID").expect("Missing ASSISTANT_ID");

    // Send a message to the assistant
    let mut create_message_request = CreateMessageRequestArgs::default().build().unwrap();
    create_message_request.content = text;
    client
        .threads()
        .messages(&thread_id)
        .create(create_message_request)
        .await
        .unwrap();

    // Create a run for the message and process it
    let mut create_run_request = CreateRunRequestArgs::default().build().unwrap();
    create_run_request.assistant_id = assistant_id;
    let run_id = client
        .threads()
        .runs(&thread_id)
        .create(create_run_request)
        .await
        .unwrap()
        .id;

    // Poll for the run's status
    let mut result = Some("Timeout");
    for _ in 0..5 {
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        let run_object = client
            .threads()
            .runs(&thread_id)
            .retrieve(run_id.as_str())
            .await
            .unwrap();
        result = match run_object.status {
            RunStatus::Queued | RunStatus::InProgress | RunStatus::Cancelling => continue,
            RunStatus::RequiresAction => Some("Action required for OpenAI assistant"),
            RunStatus::Cancelled => Some("Run is cancelled"),
            RunStatus::Failed => Some("Run has failed"),
            RunStatus::Expired => Some("Run has expired"),
            RunStatus::Completed => None,
        };
        if result.is_none() {
            break;
        }
    }

    match result {
        Some(r) => String::from(r),
        None => {
            let mut thread_messages = client
                .threads()
                .messages(&thread_id)
                .list(&[("limit", "1")])
                .await
                .unwrap();

            // Extract the last message content from the thread
            let c = thread_messages.data.pop().unwrap();
            let c = c.content.into_iter().filter_map(|x| match x {
                MessageContent::Text(t) => Some(t.text.value),
                _ => None,
            });

            c.collect()
        }
    }
}
