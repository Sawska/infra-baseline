use log::{error, warn};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct TelegramResponse {
    result: Vec<Update>,
}

#[derive(Debug, Deserialize)]
struct Update {
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    text: Option<String>,
}

/// Simple Telegram alerting client to send status updates and remote command polling.
pub struct TelegramAlert {
    chat_id: String,
    base_url: String,
}

impl TelegramAlert {
    pub fn new(chat_id: String) -> Self {
        let base_url = "http://localhost:3000".to_string();
        Self { chat_id, base_url }
    }

    /// Sends a message to the configured chat.
    pub async fn send(&self, message: &str, urgent: bool) {
        if self.chat_id.is_empty() {
            warn!("Chat ID missing, skipping alert.");
            return;
        }

        let text = if urgent {
            format!("<b>URGENT</b>\n{}", message)
        } else {
            message.to_string()
        };

        let client = reqwest::Client::new();
        let url = format!("{}/sendMessage", self.base_url);

        let payload = json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "HTML"
        });

        match client
            .post(url)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let err_text = resp.text().await.unwrap_or_default();
                    error!("Telegram API error: {}", err_text);
                }
            }
            Err(e) => error!("Failed to send Telegram alert: {}", e),
        }
    }

    /// Polls for the latest message to see if a "/stop" command was sent.
    pub async fn check_for_stop_command(&self) -> bool {
        let url = format!("{}/getUpdates", self.base_url);
        let client = reqwest::Client::new();
        let params = [("limit", "5"), ("offset", "-1")];

        match client.get(url).query(&params).send().await {
            Ok(resp) => {
                if let Ok(data) = resp.json::<TelegramResponse>().await {
                    for update in data.result {
                        if let Some(msg) = update.message
                            && let Some(text) = msg.text
                            && text.trim() == "/stop"
                        {
                            return true;
                        }
                    }
                }
            }
            Err(e) => error!("Failed to poll Telegram updates: {}", e),
        }
        false
    }
}
