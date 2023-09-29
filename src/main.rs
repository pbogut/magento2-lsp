mod indexer;
mod js;
mod lsp;
mod m2_types;
mod php;
mod ts;
mod xml;

use std::error::Error;

use anyhow::{Context, Result};
use lsp_server::{Connection, ExtractError, Message, Request, RequestId, Response};
use lsp_types::OneOf;
use lsp_types::{
    request::GotoDefinition, GotoDefinitionResponse, InitializeParams, ServerCapabilities,
};

use crate::indexer::Indexer;

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    // Note that  we must have our logging only write out to stderr.
    eprintln!("Starting magento2-ls LSP server");

    // Create the transport. Includes the stdio (stdin and stdout) versions but this could
    // also be implemented to use sockets or HTTP.
    let (connection, io_threads) = Connection::stdio();

    // Run the server and wait for the two threads to end (typically by trigger LSP Exit event).
    let server_capabilities = serde_json::to_value(ServerCapabilities {
        definition_provider: Some(OneOf::Left(true)),
        ..Default::default()
    })
    .context("Deserializing server capabilities")?;
    let initialization_params = connection.initialize(server_capabilities)?;

    main_loop(&connection, initialization_params)?;
    io_threads.join()?;

    // Shut down gracefully.
    eprintln!("shutting down server");
    Ok(())
}

fn main_loop(
    connection: &Connection,
    init_params: serde_json::Value,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let params: InitializeParams =
        serde_json::from_value(init_params).context("Deserializing initialize params")?;

    let indexer = Indexer::new().into_arc();
    let mut threads = vec![];

    if let Some(uri) = params.root_uri {
        let path = uri.to_file_path().expect("Invalid root path");
        threads.extend(Indexer::update_index(&indexer, &path));
    };

    if let Some(folders) = params.workspace_folders {
        for folder in folders {
            let path = folder.uri.to_file_path().expect("Invalid workspace path");
            threads.extend(Indexer::update_index(&indexer, &path));
        }
    }

    eprintln!("Starting main loop");
    for msg in &connection.receiver {
        #[cfg(debug_assertions)]
        eprintln!("got msg: {msg:?}");
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                #[cfg(debug_assertions)]
                eprintln!("got request: {req:?}");
                match cast::<GotoDefinition>(req) {
                    Ok((id, params)) => {
                        #[cfg(debug_assertions)]
                        eprintln!("got gotoDefinition request #{id}: {params:?}");
                        let result = Some(GotoDefinitionResponse::Array(
                            lsp::get_location_from_params(&indexer, params)
                                .map_or(vec![], |loc_list| loc_list),
                        ));

                        let result =
                            serde_json::to_value(&result).context("Error serializing response")?;
                        let resp = Response {
                            id,
                            result: Some(result),
                            error: None,
                        };
                        connection.sender.send(Message::Response(resp))?;
                        continue;
                    }
                    Err(err @ ExtractError::JsonError { .. }) => panic!("{err:?}"),
                    Err(ExtractError::MethodMismatch(req)) => req,
                };
                // ...
            }
            Message::Response(_resp) => {
                #[cfg(debug_assertions)]
                eprintln!("got response: {_resp:?}");
            }
            Message::Notification(_not) => {
                #[cfg(debug_assertions)]
                eprintln!("got notification: {_not:?}");
            }
        }
    }

    for thread in threads {
        thread.join().ok();
    }

    Ok(())
}

fn cast<R>(req: Request) -> Result<(RequestId, R::Params), ExtractError<Request>>
where
    R: lsp_types::request::Request,
    R::Params: serde::de::DeserializeOwned,
{
    req.extract(R::METHOD)
}
