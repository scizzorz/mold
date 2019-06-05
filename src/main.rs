use exitfailure::ExitFailure;
use failure::Error;
use mold::remote;
use mold::EnvMap;
use mold::Moldfile;
use mold::Recipe;
use mold::Task;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use structopt::StructOpt;

/// A fresh task runner
#[derive(StructOpt, Debug)]
#[structopt(raw(setting = "structopt::clap::AppSettings::ColoredHelp"))]
pub struct Args {
  /// Path to the moldfile
  #[structopt(long = "file", short = "f", default_value = "moldfile")]
  pub file: PathBuf,

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

  run(args)?;

  Ok(())
}

fn run(args: Args) -> Result<(), Error> {
  run_aux(args, None)?;
  Ok(())
}

fn prepare(file: &Path, update: bool) -> Result<Moldfile, Error> {
  let data = Moldfile::discover(&file)?;

  // find our mold recipe dir and create it if it doesn't exist
  let mold_dir = data.mold_dir(&file)?;
  if !mold_dir.is_dir() {
    fs::create_dir(&mold_dir)?;
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
        } else if update {
          remote::checkout(&path, &group.ref_)?;
        }
      }
    }
  }

  Ok(data)
}

fn run_aux(args: Args, prev_env: Option<&EnvMap>) -> Result<(), Error> {
  // load the moldfile
  let data = prepare(&args.file, args.update)?;

  // optionally spew the parsed structure
  if args.debug {
    dbg!(&data);
  }

  // early return if we passed a --update
  if args.update {
    return Ok(());
  }

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

  // print help if we didn't pass any targets
  if args.targets.is_empty() {
    return data.help();
  }

  // find all recipes to run
  let targets = find_all_dependencies(&args.file, &data, &args.targets)?;

  let mut tasks: Vec<Task> = vec![];

  // run all targets
  for target_name in &targets {
    tasks.push(find_task(&args.file, &data, &target_name, &env)?);
  }

  for task in &tasks {
    task.exec()?;
  }

  Ok(())
}

fn find_all_dependencies(root: &Path, data: &Moldfile, targets: &Vec<String>) -> Result<Vec<String>, Error> {
  let mut new_targets = vec![];

  // FIXME deduplicate
  for target_name in targets {
    new_targets.extend(find_dependencies(root, data, target_name)?);
    new_targets.push(target_name.to_string());
  }

  Ok(new_targets)
}

fn find_dependencies(root: &Path, data: &Moldfile, target: &str) -> Result<Vec<String>, Error> {
  if target.contains('/') {
    let splits: Vec<_> = target.splitn(2, '/').collect();
    let group_name = splits[0];
    let recipe_name = splits[1];

    let group_file = data.find_group_file(root, group_name)?;
    let group = Moldfile::open(&group_file)?;

    // FIXME this doesn't recurse properly, probably...
    // .find_recipe(...) doesn't handle subrecipes
    let recipe = group.find_recipe(recipe_name)?;
    let deps = recipe
      .dependencies()
      .iter()
      .map(|x| format!("{}/{}", group_name, x))
      .collect();

    return find_all_dependencies(&group_file, &group, &deps);
  }

  let recipe = data.find_recipe(target)?;
  let deps = recipe.dependencies();
  find_all_dependencies(root, data, &deps)
}

fn find_task(root: &Path, data: &Moldfile, target_name: &str, env: &EnvMap) -> Result<Task, Error> {
  let mold_dir = data.mold_dir(root)?;

  // check if we're executing a group subrecipe
  if target_name.contains('/') {
    let splits: Vec<_> = target_name.splitn(2, '/').collect();
    let group_name = splits[0];
    let recipe_name = splits[1];
    let group_file = data.find_group_file(root, group_name)?;
    let group = Moldfile::open(&group_file)?;
    return find_task(&group_file, &group, recipe_name, env);
  }

  // ...not executing subrecipe, so look up the top-level recipe
  let recipe = data.find_recipe(target_name)?;

  let task = match recipe {
    Recipe::Command(target) => Task::from_args(&target.command, Some(&env)),
    Recipe::Script(target) => {
      // what the interpreter is for this recipe
      let type_ = data.find_type(&target.type_)?;

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

      type_.task(&script.to_str().unwrap(), env)
    }
    Recipe::Group(_) => return Err(failure::err_msg("Can't execute a group")),
  };

  Ok(task)
}
