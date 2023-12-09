use anyhow::anyhow;
use anyhow::Result;
use ratatui::prelude::Rect;
use tokio::sync::mpsc;

use super::BubbleList;
use super::CodeBlocks;
use super::Scroll;
use super::Sessions;
use super::Themes;
use crate::domain::models::AcceptType;
use crate::domain::models::Action;
use crate::domain::models::Author;
use crate::domain::models::BackendResponse;
use crate::domain::models::EditorContext;
use crate::domain::models::Message;
use crate::domain::models::MessageType;
use crate::domain::models::SlashCommand;
use crate::infrastructure::backends::BackendManager;
use crate::infrastructure::editors::EditorManager;

#[cfg(test)]
#[path = "app_state_test.rs"]
mod tests;

pub struct AppStateProps {
    pub backend_name: String,
    pub editor_name: String,
    pub model_name: String,
    pub theme_name: String,
    pub theme_file: String,
    pub session_id: Option<String>,
}

pub struct AppState<'a> {
    pub backend_context: String,
    pub bubble_list: BubbleList<'a>,
    pub codeblocks: CodeBlocks,
    pub editor_context: Option<EditorContext>,
    pub exit_warning: bool,
    pub last_known_height: usize,
    pub last_known_width: usize,
    pub messages: Vec<Message>,
    pub scroll: Scroll,
    pub session_id: String,
    pub waiting_for_backend: bool,
}

fn editor_message(editor_context: String) -> Message {
    return Message::new(
        Author::Model,
        &format!(
            "Hey there! Let's talk about the following: \n\n{}",
            editor_context
        ),
    );
}

fn intro_message() -> Message {
    return Message::new(Author::Model, "Hey there! What can I do for you?");
}

impl<'a> AppState<'a> {
    pub async fn new(props: AppStateProps) -> Result<AppState<'a>> {
        if props.session_id.is_some() {
            return AppState::from_session(props).await;
        }

        return AppState::init(props).await;
    }

    async fn init(props: AppStateProps) -> Result<AppState<'a>> {
        let backend_name = &props.backend_name;
        let model_name = &props.model_name;
        let theme = Themes::get(&props.theme_name, &props.theme_file)?;

        let mut app_state = AppState {
            backend_context: "".to_string(),
            bubble_list: BubbleList::new(theme),
            codeblocks: CodeBlocks::default(),
            editor_context: None,
            exit_warning: false,
            last_known_height: 0,
            last_known_width: 0,
            messages: vec![],
            scroll: Scroll::default(),
            session_id: Sessions::create_id(),
            waiting_for_backend: false,
        };

        let backend = BackendManager::get(backend_name)?;
        if let Err(err) = backend.health_check().await {
            app_state
                .messages
                .push(Message::new_with_type(
                    Author::Oatmeal,
                    MessageType::Error,
                    &format!("Hey, it looks like backend {backend_name} isn't running, I can't connect to it. You should double check that before we start talking, otherwise I may crash.\n\nError: {err}"),
                ));
        } else {
            let models = backend.list_models().await?;
            if !models.contains(&model_name.to_string()) {
                app_state
                .messages
                .push(Message::new_with_type(
                    Author::Oatmeal,
                    MessageType::Error,
                    format!("Model {model_name} doesn't exist for backend {backend_name}. You can use `/modellist` to view all avaiable models, and `/model NAME` to switch models.").as_str(),
                ));
            }
        }

        // Fallback to the default intro message when there's no editor context.
        if app_state
            .add_editor_context(&props.editor_name)
            .await
            .is_err()
        {
            app_state.messages.push(intro_message());
        }

        return Ok(app_state);
    }

    async fn from_session(props: AppStateProps) -> Result<AppState<'a>> {
        let session_id = props.session_id.clone().unwrap().to_string();
        let session = Sessions::default().load(&session_id).await?;
        let theme = Themes::get(&props.theme_name, &props.theme_file)?;

        let mut app_state = AppState {
            backend_context: session.state.backend_context,
            bubble_list: BubbleList::new(theme),
            codeblocks: CodeBlocks::default(),
            editor_context: None,
            exit_warning: false,
            last_known_height: 0,
            last_known_width: 0,
            messages: session.state.messages,
            scroll: Scroll::default(),
            session_id,
            waiting_for_backend: false,
        };

        app_state
            .codeblocks
            .replace_from_messages(&app_state.messages);

        if let Ok(editor) = EditorManager::get(&props.editor_name) {
            if editor.health_check().await.is_ok() {
                app_state.editor_context = editor.get_context().await?;
            }
        }

        return Ok(app_state);
    }

    async fn add_editor_context(&mut self, editor_name: &str) -> Result<()> {
        if editor_name.is_empty() {
            return Err(anyhow!("Editor name not set."));
        }

        let editor_res = EditorManager::get(editor_name);
        if editor_res.is_err() {
            return Err(anyhow!("Failed to load editor from manager"));
        }

        let editor = editor_res.unwrap();
        if let Err(err) = editor.health_check().await {
            self
                .messages
                .push(Message::new_with_type(
                    Author::Oatmeal,
                    MessageType::Error,
                    &format!("Whoops, it looks like editor {editor_name} isn't setup properly. You should double check that before we start talking, otherwise I may crash.\n\nError: {err}"),
                ));

            return Ok(());
        }

        if let Some(editor_context) = editor.get_context().await? {
            let formatted = editor_context.format();
            self.editor_context = Some(editor_context);
            self.messages.push(editor_message(formatted));

            return Ok(());
        } else {
            return Err(anyhow!("No editor context"));
        }
    }

    pub fn reset_state(&mut self, clear_context: bool) {
        self.backend_context = "".to_string();
        self.exit_warning = false;
        self.last_known_width = 0;
        self.last_known_height = 0;
        self.messages = vec![];
        self.session_id = Sessions::create_id();
        self.scroll = Scroll::default();

        if clear_context {
            self.editor_context = None;
        }

        if self.editor_context.is_some() {
            self.messages.push(editor_message(
                self.editor_context.clone().unwrap().format(),
            ))
        } else {
            self.messages.push(intro_message());
        }
    }

    pub fn handle_backend_response(&mut self, msg: BackendResponse) {
        let last_message = self.messages.last_mut().unwrap();
        if last_message.author != Author::User {
            last_message.append(&msg.text);
        } else {
            self.messages.push(Message::new(msg.author, &msg.text));
        }

        self.sync_dependants();

        if msg.done {
            self.waiting_for_backend = false;
            if let Some(ctx) = msg.context {
                self.backend_context = ctx;
            }

            if self.backend_context.is_empty() {
                self.add_message(Message::new_with_type(
                    Author::Oatmeal,
                    MessageType::Error,
                    "Error: No context was provided by the backend upon completion. Please report this bug on Github."
                ));
                self.sync_dependants();
            }

            self.codeblocks.replace_from_messages(&self.messages);
        }
    }

    pub fn handle_slash_commands(
        &mut self,
        input_str: &str,
        tx: &mpsc::UnboundedSender<Action>,
    ) -> Result<(bool, bool)> {
        let mut should_break = false;
        let mut should_continue = false;

        if let Some(command) = SlashCommand::parse(input_str) {
            if command.is_quit() {
                should_break = true;
            }

            if command.is_append_code_block()
                || command.is_replace_code_block()
                || command.is_copy_code_block()
            {
                should_continue = true;
                let codeblocks_res = self.codeblocks.blocks_from_slash_commands(&command);
                if let Err(err) = codeblocks_res.as_ref() {
                    self.add_message(Message::new_with_type(
                        Author::Oatmeal,
                        MessageType::Error,
                        &format!(
                            "There was an error trying to parse your command:\n\n{:?}",
                            err
                        ),
                    ));

                    return Ok((should_break, should_continue));
                }

                if command.is_copy_code_block() {
                    tx.send(Action::CopyMessages(vec![Message::new(
                        Author::Model,
                        &codeblocks_res.unwrap(),
                    )]))?;
                    self.waiting_for_backend = true;
                    return Ok((should_break, should_continue));
                }

                let mut accept_type = AcceptType::Append;
                if command.is_replace_code_block() {
                    accept_type = AcceptType::Replace;
                }

                tx.send(Action::AcceptCodeBlock(
                    self.editor_context.clone(),
                    codeblocks_res.unwrap(),
                    accept_type,
                ))?;
            }

            if command.is_copy_chat() {
                should_continue = true;
                tx.send(Action::CopyMessages(self.messages.clone()))?;
                self.waiting_for_backend = true;
            }

            // Reset backend context on model switch.
            if command.is_model_set() {
                self.backend_context = "".to_string();
            }

            if command.is_new() {
                self.reset_state(!command.args.is_empty() && command.args[0] == "clear");
                should_continue = true;
            }
        }

        return Ok((should_break, should_continue));
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.last_known_width = rect.width.into();
        self.last_known_height = rect.height.into();
        self.sync_dependants();
    }

    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
        self.sync_dependants();
        self.scroll.last();
    }

    fn sync_dependants(&mut self) {
        self.bubble_list
            .set_messages(&self.messages, self.last_known_width);

        self.scroll
            .set_state(self.bubble_list.len(), self.last_known_height);

        if self.waiting_for_backend {
            self.scroll.last();
        }
    }

    pub async fn save_session(&self) -> Result<()> {
        Sessions::default()
            .save(
                &self.session_id,
                &self.backend_context,
                &self.editor_context,
                &self.messages,
            )
            .await?;

        return Ok(());
    }
}
