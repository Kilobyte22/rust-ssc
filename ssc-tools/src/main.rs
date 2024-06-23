use std::collections::HashMap;

use clap::builder::PossibleValuesParser;
use clap::ArgMatches;
use serde_json::Value;

use ssc::{ListNode, Protocol};

fn args() -> clap::Command {
    clap::command!()
        .arg(
            clap::Arg::new("server")
                .short('s')
                .help("Server to connect to, disabled discovery")
                .required(true),
        )
        .arg(
            clap::Arg::new("port")
                .short('p')
                .help("Port to connect to")
                .required(true)
                .value_parser(clap::value_parser!(u16)),
        )
        .arg(
            clap::Arg::new("protocol")
                .short('P')
                .help("L4 Protocol, currently supported TCP and UDP")
                .required(true)
                .default_value("TCP")
                .value_parser(clap::builder::PossibleValuesParser::new(&["TCP", "UDP"])),
        )
        .subcommand(
            clap::Command::new("get")
                .about("Gets the contents of a certain SSC path")
                .arg(
                    clap::Arg::new("output")
                        .short('o')
                        .long("output-format")
                        .value_parser(PossibleValuesParser::new(&["literal", "json"]))
                        .default_value("literal"),
                )
                .arg(
                    clap::Arg::new("PATH")
                        .help("The path to retrieve the value for")
                        .required(true),
                ),
        )
        .subcommand(
            clap::Command::new("set")
                .about("Sets the contents of a certain SSC path")
                .arg(
                    clap::Arg::new("input")
                        .short('i')
                        .long("input-format")
                        .value_parser(PossibleValuesParser::new(&[
                            "string", "number", "boolean", "json",
                        ]))
                        .default_value("string"),
                )
                .arg(
                    clap::Arg::new("PATH")
                        .help("The path to retrieve the value for")
                        .required(true),
                )
                .arg(
                    clap::Arg::new("VALUE")
                        .help("The value to set, the format is specified using -i")
                        .required(true),
                ),
        )
        .subcommand(clap::Command::new("discover").about("Find all devices to be discovered"))
        .subcommand(clap::Command::new("list").about("Lists all available paths"))
        .subcommand_required(true)
}

async fn get_client_for_args(args: &ArgMatches) -> ssc::error::Result<ssc::Client> {
    let host = args.get_one::<String>("server").unwrap().as_str();
    let port: u16 = *args.get_one("port").unwrap();
    let protocol = match args.get_one::<String>("protocol").unwrap().as_str() {
        "TCP" => Protocol::TCP,
        "UDP" => Protocol::UDP,
        _ => unreachable!(),
    };
    Ok(ssc::Client::connect((host, port), protocol).await?)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = args().get_matches();

    if let Some(("discover", _)) = args.subcommand() {
        let mut discovery = ssc::discover().await;

        loop {
            let device = discovery.next().await;

            println!(
                "Found A {:?} with serial number {:?}",
                device.model, device.id
            );
        }
    }

    let mut client = get_client_for_args(&args).await?;

    match args.subcommand() {
        Some(("get", matches)) => {
            let path = matches.get_one::<String>("PATH").unwrap().as_str();

            let value: serde_json::Value = client.get(&path).await?;

            println!("{}", display_output(&value, &matches)?);
        }
        Some(("set", matches)) => {
            let path = matches.get_one::<String>("PATH").unwrap().as_str();
            let value = matches.get_one::<String>("VALUE").unwrap().as_str();

            let value = parse_input(value, &matches)?;

            client.set(path, &value).await?;
        }
        Some(("list", _)) => {
            let tree = client.list("/").await?;

            print_list_tree(&tree, "");
        }
        _ => unreachable!(),
    };
    Ok(())
}

fn print_list_tree(tree: &HashMap<String, ListNode>, indent: &str) {
    for (key, value) in tree {
        println!("{indent}/{key}");
        match value {
            ListNode::Branch(hm) => {
                print_list_tree(hm, &format!("{indent}    "));
            }
            ListNode::Leaf => {}
        }
    }
}

fn display_output(value: &serde_json::Value, matches: &ArgMatches) -> anyhow::Result<String> {
    match matches.get_one::<String>("output").unwrap().as_str() {
        "literal" => Ok(match value {
            Value::Null => "null".to_owned(),
            Value::Bool(value) => value.to_string(),
            Value::Number(value) => value.to_string(),
            Value::String(value) => value.to_owned(),
            Value::Array(_) => return Err(anyhow::Error::msg(
                "Result is an array which can't be displayed as literal. Please specify `-o json`",
            )),
            Value::Object(_) => return Err(anyhow::Error::msg(
                "Result is an object which can't be displayed as literal. Please specify `-o json`",
            )),
        }),
        "json" => Ok(serde_json::to_string(value).unwrap()),
        _ => unreachable!(),
    }
}

fn parse_input(value: &str, matches: &ArgMatches) -> anyhow::Result<serde_json::Value> {
    Ok(match matches.get_one::<String>("input").unwrap().as_str() {
        "string" => serde_json::Value::String(value.to_owned()),
        "number" => {
            serde_json::Value::Number(serde_json::Number::from_f64(value.parse()?).unwrap())
        }
        "boolean" => serde_json::Value::Bool(value == "true"),
        "json" => serde_json::from_str(value)?,
        _ => unreachable!(),
    })
}
