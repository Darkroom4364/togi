use clap::Parser;

mod cli;
mod config;
mod diff;
mod languages;
mod mapper;
mod mutator;
mod operators;
mod parser;
mod report;
mod runner;

fn main() {
    let _args = cli::Cli::parse();
    todo!("wire up pipeline")
}
