use colored::*;
use exitfailure::ExitFailure;
use failure::Error;
use mold::remote;
use mold::EnvMap;
use mold::Moldfile;
use mold::Recipe;
use std::fs;
use std::fs::File;
use std::io::prelude::*;
use structopt::StructOpt;

/// A fresh task runner
#[derive(StructOpt, Debug)]
#[structopt(raw(setting = "structopt::clap::AppSettings::ColoredHelp"))]
pub struct Args {
  /// Path to the moldfile
  #[structopt(long = "file", short = "f", default_value = "moldfile")]
  pub file: std::path::PathBuf,

  /// Don't print extraneous information
  #[structopt(long = "quiet", short = "q")]
  pub quiet: bool,

  /// dbg! the parsed moldfile
  #[structopt(long = "debug", short = "d")]
  pub debug: bool,

  #[structopt(long = "update", short = "u")]
  pub update: bool,

  /// Which recipe(s) to run
  pub targets: Vec<String>,
}

fn main() -> Result<(), ExitFailure> {
  let args = Args::from_args();
  env_logger::init();

  run(args, None)?;

  Ok(())
}

fn print_help(data: &Moldfile) -> Result<(), Error> {
  for (name, recipe) in &data.recipes {
    let (name, help) = match recipe {
      Recipe::Command(c) => (name.yellow(), &c.help),
      Recipe::Script(s) => (name.cyan(), &s.help),
      Recipe::Group(g) => (format!("{}/", name).magenta(), &g.help),
    };
    println!("{:>12} {}", name, help);
  }

  Ok(())
}

fn run(args: Args, prev_env: Option<&EnvMap>) -> Result<(), Error> {
  // read and deserialize the moldfile
  // FIXME this should probably do a "discover"-esque thing and crawl up the tree
  // looking for one
  let mut file = File::open(&args.file)?;
  let mut contents = String::new();
  file.read_to_string(&mut contents)?;
  let data: Moldfile = toml::de::from_str(&contents)?;

  // merge this moldfile's environment with its parent.
  // the parent has priority and overrides this moldfile because it's called recursively:
  //   $ mold foo/bar/baz
  // will call bar/baz with foo as the parent, which will call baz with bar as
  // the parent.  we want foo's moldfile to override bar's moldfile to override
  // baz's moldfile, because baz should be the least specialized.
  let mut env = data.environment.clone();
  if let Some(prev_env) = prev_env {
    env.extend(prev_env.into_iter().map(|(k, v)| (k.clone(), v.clone())));
  }

  // optionally spew the parsed structure
  if args.debug {
    dbg!(&data);
  }

  // find our mold recipe dir and create it if it doesn't exist
  let mut mold_dir = args.file.clone();
  mold_dir.pop();
  mold_dir.push(&data.recipe_dir);
  let mold_dir = fs::canonicalize(mold_dir)?;

  if !mold_dir.is_dir() {
    fs::create_dir(&mold_dir)?;
  }

  // debug dump the moldfile
  if args.debug {
    dbg!(&mold_dir);
  }

  // clone or update all of our remotes if we haven't already
  for (name, recipe) in &data.recipes {
    match recipe {
      Recipe::Command(_) => {}
      Recipe::Script(_) => {}
      Recipe::Group(group) => {
        let mut path = mold_dir.clone();
        path.push(name);

        if !path.is_dir() {
          remote::clone(&group.url, &path)?;
          remote::checkout(&path, &group.ref_)?;
        } else if args.update {
          remote::checkout(&path, &group.ref_)?;
        }
      }
    }
  }

  // early return if we passed a --update
  if args.update {
    return Ok(());
  }

  // print help if we didn't pass any targets
  if args.targets.is_empty() {
    return print_help(&data);
  }

  // run all targets
  for target_name in &args.targets {
    run_target(&args, &data, &target_name, &env)?;
  }

  Ok(())
}

fn run_target(args: &Args, data: &Moldfile, target_name: &str, env: &EnvMap) -> Result<(), Error> {
  // print help if our target is an empty string
  // FIXME this feels wrong
  if target_name.is_empty() {
    return print_help(&data);
  }

  // FIXME this feels like it shouldn't need to be recomputed, but... meh.
  let mut mold_dir = args.file.clone();
  mold_dir.pop();
  mold_dir.push(&data.recipe_dir);
  let mold_dir = fs::canonicalize(mold_dir)?;

  // check if we're executing a group subrecipe
  if target_name.contains('/') {
    let splits: Vec<_> = target_name.splitn(2, '/').collect();
    let group_name = splits[0];
    let recipe_name = splits[1];

    let target = data
      .recipes
      .get(group_name)
      .ok_or_else(|| failure::err_msg("couldn't locate target group"))?;

    // unwrap the group or quit
    let target = match target {
      Recipe::Script(_) => return Err(failure::err_msg("Can't execute a subrecipe of a script")),
      Recipe::Command(_) => return Err(failure::err_msg("Can't execute a subrecipe of a command")),
      Recipe::Group(target) => target,
    };

    // recurse down the line
    let new_args = Args {
      file: mold_dir.join(group_name).join(&target.file),
      targets: vec![recipe_name.to_string()],
      ..*args
    };
    return run(new_args, Some(env));
  }

  // ...not executing subrecipe, so look up the top-level recipe
  let target = data
    .recipes
    .get(target_name)
    .ok_or_else(|| failure::err_msg("couldn't locate target"))?;

  match target {
    Recipe::Command(target) => {
      // this is some weird witchcraft to turn a Vec<String> into a Vec<&str>
      mold::exec(target.command.iter().map(AsRef::as_ref).collect(), env)?;
    }
    Recipe::Script(target) => {
      // what the interpreter is for this recipe
      let type_ = data
        .types
        .get(&target.type_)
        .ok_or_else(|| failure::err_msg("couldn't locate type"))?;

      // find the script file to execute
      let script = match &target.script {
        Some(x) => {
          let mut path = mold_dir.clone();
          path.push(x);
          path
        }

        // we need to look it up based on our interpreter's known extensions
        None => type_.find(&mold_dir, &target_name)?,
      };

      type_.exec(&script.to_str().unwrap(), env)?;
    }
    Recipe::Group(_) => return Err(failure::err_msg("Can't execute a group")),
  };

  Ok(())
}
