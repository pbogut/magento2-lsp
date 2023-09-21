mod php;
mod ts;
mod xml;
use lsp_server::{Connection, ExtractError, Message, Request, RequestId, Response};
use lsp_types::{
    request::GotoDefinition, GotoDefinitionResponse, InitializeParams, ServerCapabilities,
};
use lsp_types::{GotoDefinitionParams, Location, OneOf};
use php::*;
use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;

#[derive(Debug, Clone)]
struct Indexer {
    php_classes: HashMap<String, PHPClass>,
    magento_modules: HashMap<String, PathBuf>,
}

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    // Note that  we must have our logging only write out to stderr.
    eprintln!("Starting magento2-ls LSP server");

    // Create the transport. Includes the stdio (stdin and stdout) versions but this could
    // also be implemented to use sockets or HTTP.
    let (connection, io_threads) = Connection::stdio();

    // Run the server and wait for the two threads to end (typically by trigger LSP Exit event).
    let server_capabilities = serde_json::to_value(&ServerCapabilities {
        definition_provider: Some(OneOf::Left(true)),
        ..Default::default()
    })
    .unwrap();
    let initialization_params = connection.initialize(server_capabilities)?;

    main_loop(connection, initialization_params)?;
    io_threads.join()?;

    // Shut down gracefully.
    eprintln!("shutting down server");
    Ok(())
}

fn main_loop(
    connection: Connection,
    init_params: serde_json::Value,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let params: InitializeParams = serde_json::from_value(init_params).unwrap();

    let map: HashMap<String, PHPClass> = HashMap::new();

    eprint!("Preparing index...");
    let root_path = params.root_uri.as_ref().unwrap().path();
    let mut indexer = Indexer {
        php_classes: map,
        magento_modules: php::get_modules_map(&PathBuf::from(root_path)),
    };
    eprintln!(" done");

    eprintln!("Starting main loop");
    for msg in &connection.receiver {
        eprintln!("got msg: {msg:?}");
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                eprintln!("got request: {req:?}");
                match cast::<GotoDefinition>(req) {
                    Ok((id, params)) => {
                        eprintln!("got gotoDefinition request #{id}: {params:?}");
                        let result = match get_location_from_params(&mut indexer, params) {
                            Some(loc) => Some(GotoDefinitionResponse::Array(vec![loc])),
                            None => Some(GotoDefinitionResponse::Array(vec![])),
                        };
                        let result = serde_json::to_value(&result).unwrap();
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
            Message::Response(resp) => {
                eprintln!("got response: {resp:?}");
            }
            Message::Notification(not) => {
                eprintln!("got notification: {not:?}");
            }
        }
    }
    Ok(())
}

fn get_module_path(index: &Indexer, class: &str) -> Option<(PathBuf, Vec<String>)> {
    let mut parts = class.split("\\").collect::<Vec<_>>();
    let mut suffix = vec![];

    while let Some(part) = parts.pop() {
        suffix.push(part.to_string());
        let prefix = parts.join("\\");

        match index.magento_modules.get(&prefix) {
            Some(mod_path) => {
                suffix.reverse();
                return Some((mod_path.clone(), suffix));
            }
            None => continue,
        }
    }

    None
}

fn get_php_class_from_class_name(index: &mut Indexer, class: &str) -> Option<PHPClass> {
    match index.php_classes.get(class) {
        Some(phpclass) => Some(phpclass.clone()),
        None => {
            match get_module_path(index, &class) {
                None => None,
                Some((mut file_path, suffix)) => {
                    suffix.iter().for_each(|part| {
                        file_path.push(part);
                    });
                    file_path.set_extension("php");

                    match file_path.try_exists() {
                        Ok(true) => {
                            let phpclass = parse_php_file(file_path.clone())?;
                            // update indexer for future use
                            index
                                .php_classes
                                .insert(class.to_string(), phpclass.clone());
                            Some(phpclass)
                        }
                        _ => None,
                    }
                }
            }
        }
    }
}

fn get_location_from_params(index: &mut Indexer, params: GotoDefinitionParams) -> Option<Location> {
    let uri = params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;

    match xml::get_item_from_position(uri, pos) {
        Some(M2Item::Class(class)) => {
            let phpclass = get_php_class_from_class_name(index, &class)?;
            index.php_classes.insert(class.clone(), phpclass.clone());
            Some(Location {
                uri: phpclass.uri.clone(),
                range: phpclass.range.clone(),
            })
        }
        Some(M2Item::Method(class, method)) => {
            let phpclass = get_php_class_from_class_name(index, &class)?;
            index.php_classes.insert(class.clone(), phpclass.clone());

            Some(Location {
                uri: phpclass.uri.clone(),
                range: phpclass
                    .methods
                    .get(&method)
                    .map_or(phpclass.range, |method| method.range),
            })
        }
        Some(M2Item::Const(class, constant)) => {
            let phpclass = get_php_class_from_class_name(index, &class)?;
            index.php_classes.insert(class.clone(), phpclass.clone());

            Some(Location {
                uri: phpclass.uri.clone(),
                range: phpclass
                    .constants
                    .get(&constant)
                    .map_or(phpclass.range, |method| method.range),
            })
        }
        None => None,
    }
}

fn cast<R>(req: Request) -> Result<(RequestId, R::Params), ExtractError<Request>>
where
    R: lsp_types::request::Request,
    R::Params: serde::de::DeserializeOwned,
{
    req.extract(R::METHOD)
}
