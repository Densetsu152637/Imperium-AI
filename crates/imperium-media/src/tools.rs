//! The session's media tools as a [`ToolProvider`]: `media_image`, `media_speak`, and
//! `media_transcribe`, for the configured backends.

use std::sync::Arc;

use imperium_api::{
    ToolCheck, ToolContext, ToolEvent, ToolFuture, ToolProvider, ToolSpec, parse_arguments,
    truncate_chars,
};
use serde::Deserialize;

use crate::Media;

/// Media tools for a session. Every call asks first (it may cost money or send data to a
/// service) unless the user allowed the tool for the session.
#[derive(Debug, Clone)]
pub struct MediaTools {
    media: Arc<Media>,
    http: reqwest::Client,
}

impl MediaTools {
    pub fn new(media: Arc<Media>) -> Self {
        MediaTools {
            media,
            http: reqwest::Client::new(),
        }
    }
}

#[derive(Deserialize)]
struct Args {
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    voice: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    path: String,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

impl ToolProvider for MediaTools {
    fn specs(&self) -> Vec<ToolSpec> {
        self.media.tool_specs()
    }

    fn handles(&self, name: &str) -> bool {
        matches!(name, "media_image" | "media_speak" | "media_transcribe")
    }

    fn check(&self, name: &str, arguments: &str) -> Result<ToolCheck, String> {
        let args: Args = parse_arguments(arguments)?;
        let summary = match name {
            "media_image" => format!("generate an image: {}", truncate_chars(&args.prompt, 200)),
            "media_speak" => format!("speak: {}", truncate_chars(&args.text, 200)),
            _ => format!("transcribe {}", args.path),
        };
        Ok(ToolCheck {
            summary,
            ask: true,
            needs_context: false,
            command: None,
        })
    }

    fn call<'a>(
        &'a self,
        context: &'a ToolContext<'a>,
        name: &'a str,
        arguments: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Args = parse_arguments(arguments)?;
            let (media, http, root) = (&self.media, &self.http, context.root);
            let result = match name {
                "media_image" => {
                    let size = (args.width.unwrap_or(1024), args.height.unwrap_or(1024));
                    media
                        .image(http, root, &args.prompt, size, args.name.as_deref())
                        .await
                        .map(|path| {
                            (context.emit)(ToolEvent::MediaCreated {
                                path: path.clone(),
                                kind: "image".into(),
                                prompt: args.prompt.clone(),
                            });
                            format!("saved {path}")
                        })
                }
                "media_speak" => media
                    .speak(
                        http,
                        root,
                        &args.text,
                        args.voice.as_deref(),
                        args.name.as_deref(),
                    )
                    .await
                    .map(|path| {
                        (context.emit)(ToolEvent::MediaCreated {
                            path: path.clone(),
                            kind: "speech".into(),
                            prompt: args.text.clone(),
                        });
                        format!("saved {path}")
                    }),
                _ => media.transcribe(http, root, &args.path).await,
            };
            result.map_err(|error| error.to_string())
        })
    }
}
