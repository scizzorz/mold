pub mod expr;
pub mod file;
pub mod remote;
pub mod util;

use colored::*;
use failure::Error;
use file::Moldfile;
use file::Recipe;
use file::Remote;
use file::TargetSet;
use file::VarMap;
use file::DEFAULT_FILES;
use indexmap::IndexMap;
use semver::Version;
use semver::VersionReq;
use std::collections::HashSet;
use std::fs;
use std::io::prelude::*;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::string::ToString;

/// Generate a list of all active environments
///
/// Environment map keys are parsed as test expressions and evaluated against
/// the list of environments. Environments that evaluate to true are added to
/// the returned list; environments that evaluate to false are ignored.
fn active_envs(env_map: &file::EnvMap, envs: &[String]) -> Vec<String> {
  let mut result = vec![];
  for (test, _) in env_map {
    match expr::compile(&test) {
      Ok(ex) => {
        if ex.apply(&envs) {
          result.push(test.clone());
        }
      }
      // FIXME this error handling should probably be better
      Err(err) => println!("{}: '{}': {}", "Warning".bright_red(), test, err),
    }
  }
  result
}

#[derive(Debug)]
pub struct Mold {
  /// path to the moldfile
  file: PathBuf,

  /// path to the recipe scripts
  dir: PathBuf,

  /// (derived) root directory that the file sits in
  root_dir: PathBuf,

  /// (derived) path to the cloned repos
  clone_dir: PathBuf,

  /// (derived) path to the generated scripts
  script_dir: PathBuf,

  /// which environments to use in the environment
  envs: Vec<String>,

  /// the parsed moldfile data
  data: file::Moldfile,
}

// Moldfiles
impl Mold {
  /// Open a moldfile and load it
  pub fn open(path: &Path) -> Result<Mold, Error> {
    let mut file = fs::File::open(path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    let data: Moldfile = serde_yaml::from_str(&contents)?;
    let self_version = Version::parse(clap::crate_version!())?;
    let target_version = VersionReq::parse(&data.version)?;

    if !target_version.matches(&self_version) {
      return Err(failure::format_err!(
        "Incompatible versions: file {} requires version {}, but current version is {}",
        path.to_str().unwrap().blue(),
        target_version.to_string().green(),
        self_version.to_string().red()
      ));
    }

    let dir = path.with_file_name(&data.recipe_dir);
    let root_dir = dir.parent().unwrap_or(&Path::new("/")).to_path_buf();
    let clone_dir = dir.join(".clones");
    let script_dir = dir.join(".scripts");

    if !dir.is_dir() {
      fs::create_dir(&dir)?;
    }
    if !clone_dir.is_dir() {
      fs::create_dir(&clone_dir)?;
    }
    if !script_dir.is_dir() {
      fs::create_dir(&script_dir)?;
    }

    Ok(Mold {
      file: fs::canonicalize(path)?,
      dir: fs::canonicalize(dir)?,
      root_dir: fs::canonicalize(root_dir)?,
      clone_dir: fs::canonicalize(clone_dir)?,
      script_dir: fs::canonicalize(script_dir)?,
      envs: vec![],
      data,
    })
  }

  /// Try to locate a moldfile by walking up the directory tree
  fn locate_file(name: &Path) -> Result<PathBuf, Error> {
    if name.is_absolute() {
      if name.is_file() {
        return Ok(name.to_path_buf());
      } else {
        let name = format!("{}", name.display());
        return Err(failure::format_err!("File '{}' does not exist", name.red()));
      }
    }

    let mut path = std::env::current_dir()?;
    while !path.join(name).is_file() {
      path.pop();
      if path.parent().is_none() {
        break;
      }
    }

    path.push(name);

    if path.is_file() {
      Ok(path)
    } else {
      let name = format!("{}", name.display());
      Err(failure::format_err!("Unable to discover '{}'", name.red()))
    }
  }

  /// Try to locate and open a moldfile by directory
  ///
  /// Checks for DEFAULT_FILES
  fn discover_dir(name: &Path) -> Result<Mold, Error> {
    let path = DEFAULT_FILES
      .iter()
      .find_map(|file| Self::locate_file(&name.join(file)).ok())
      .ok_or_else(|| {
        failure::format_err!(
          "Cannot locate moldfile, tried the following:\n{}",
          DEFAULT_FILES.join(" ").red()
        )
      })?;
    Self::open(&path)
  }

  /// Try to locate and open a moldfile by name
  fn discover_file(name: &Path) -> Result<Mold, Error> {
    let path = Self::locate_file(name)?;
    Self::open(&path)
  }

  /// Try to locate a file or a directory
  pub fn discover(dir: &Path, file: Option<PathBuf>) -> Result<Mold, Error> {
    // I think this should take Option<&Path> but I couldn't figure out how to
    // please the compiler when I have an existing Option<PathBuf>, so...  I'm
    // just using .clone() on it.
    match file {
      Some(file) => Self::discover_file(&dir.join(file)),
      None => Self::discover_dir(dir),
    }
  }
}

// Environments
impl Mold {
  /// Return this moldfile's variables with activated environments
  ///
  /// This also inserts a few special mold variables
  pub fn env_vars(&self) -> VarMap {
    let active = active_envs(&self.data.environments, &self.envs);

    let mut vars = self.data.variables.clone();
    for env_name in active {
      if let Some(env) = self.data.environments.get(&env_name) {
        vars.extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
      }
    }

    vars
  }

  pub fn set_envs(&mut self, env: Option<String>) {
    self.envs = match env {
      Some(envs) => envs.split(',').map(|x| x.into()).collect(),
      None => vec![],
    };
  }

  pub fn add_envs(&mut self, envs: Vec<String>) {
    self.envs.extend(envs);
  }

  pub fn add_env(&mut self, env: &str) {
    self.envs.push(env.into());
  }
}

// Recipes
impl Mold {
  /// Find a recipe in the top level map
  pub fn find_recipe(&self, target_name: &str) -> Result<&Recipe, Error> {
    self
      .data
      .recipes
      .get(target_name)
      .ok_or_else(|| failure::format_err!("Couldn't locate target '{}'", target_name.red()))
  }

  fn open_remote(&self, target: &Remote) -> Result<Mold, Error> {
    let path = self.clone_dir.join(&target.folder_name());
    let mut mold = Self::discover(&path, target.file.clone())?.adopt(self);
    mold.process_includes()?;
    Ok(mold)
  }

  pub fn find_all_dependencies(&self, targets: &TargetSet) -> Result<TargetSet, Error> {
    let mut new_targets = TargetSet::new();

    for target_name in targets {
      new_targets.extend(self.find_dependencies(target_name)?);
      new_targets.insert(target_name.clone());
    }

    Ok(new_targets)
  }

  fn find_dependencies(&self, target_name: &str) -> Result<TargetSet, Error> {
    let recipe = self.find_recipe(target_name)?;
    let deps = recipe.deps().iter().map(ToString::to_string).collect();
    self.find_all_dependencies(&deps)
  }

  /// Execute a recipe
  pub fn execute(&self, target_name: &str) -> Result<(), Error> {
    let recipe = self.find_recipe(target_name)?;

    let mut vars = self.env_vars();
    vars.extend(
      self
        .mold_vars(target_name)?
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string())),
    );

    if let Some(args) = self.build_args(target_name)? {
      if args.is_empty() {
        return Err(failure::err_msg("empty command cannot be executed"));
      }

      let mut command = process::Command::new(&args[0]);
      command.args(&args[1..]);
      command.envs(vars);

      // FIXME this should be relative to root, no?
      if let Some(dir) = recipe.work_dir() {
        command.current_dir(dir);
      }

      let exit_status = command.spawn().and_then(|mut handle| handle.wait())?;

      if !exit_status.success() {
        return Err(failure::err_msg("recipe returned non-zero exit status"));
      }
    }

    Ok(())
  }

  /// Return a list of arguments to pass to Command
  pub fn build_args(&self, target_name: &str) -> Result<Option<Vec<String>>, Error> {
    let target = self.find_recipe(target_name)?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".into());
    Ok(Some(vec![shell, "-c".into(), target.shell.clone()]))
  }

  /// Return a list of arguments to pass to Command
  pub fn mold_vars(&self, target_name: &str) -> Result<VarMap, Error> {
    let mut vars = IndexMap::new();

    vars.insert("MOLD_ROOT", self.root_dir.to_string_lossy());
    vars.insert("MOLD_FILE", self.file.to_string_lossy());
    vars.insert("MOLD_DIR", self.dir.to_string_lossy());
    vars.insert("MOLD_CLONE_DIR", self.clone_dir.to_string_lossy());
    vars.insert("MOLD_SCRIPT_DIR", self.script_dir.to_string_lossy());

    if let Some(script) = self.script_name(target_name)? {
      // what the fuck is going on here?
      // PathBuf -> String is such a nightmare.
      // it seems like the .to_string().into() is needed to satisfy borrowck.
      vars.insert("MOLD_SCRIPT", script.to_string_lossy().to_string().into());
    }

    let ret: VarMap = vars
      .iter()
      .map(|(k, v)| (k.to_string(), v.to_string()))
      .collect();
    Ok(ret)
  }

  /// Return a list of arguments to pass to Command
  pub fn script_name(&self, target_name: &str) -> Result<Option<PathBuf>, Error> {
    let target = self.find_recipe(target_name)?;
    if let Some(script) = &target.script {
      let file = self.script_dir.join(util::hash_string(&script));
      fs::write(&file, &script)?;
      Ok(Some(file))
    } else {
      Ok(None)
    }
  }
}

// Remotes
impl Mold {
  /// Clone a single remote reference and then recursively clone subremotes
  fn clone(
    &self,
    folder_name: &str,
    url: &str,
    ref_: &str,
    file: Option<PathBuf>,
  ) -> Result<(), Error> {
    let path = self.clone_dir.join(folder_name);
    if !path.is_dir() {
      remote::clone(&format!("https://{}", url), &path).or_else(|_| remote::clone(url, &path))?;
      remote::checkout(&path, ref_)?;

      // open it and recursively clone its remotes
      Self::discover(&path, file.clone())?
        .adopt(self)
        .clone_all()?;
    }

    Ok(())
  }

  /// Clone a single remote
  pub fn clone_remote(&self, include: &Remote) -> Result<(), Error> {
    self.clone(
      &include.folder_name(),
      &include.url,
      &include.ref_,
      include.file.clone(),
    )
  }

  /// Recursively all Includes and Modules
  pub fn clone_all(&self) -> Result<(), Error> {
    for include in &self.data.includes {
      self.clone_remote(&include.remote)?;
    }

    Ok(())
  }

  /// Update a single remote
  ///
  /// * find the expected path
  /// * make sure it exists (ie, is cloned) and hasn't been visited
  /// * track it as visited
  /// * fetch / checkout
  /// * recurse into it
  fn update_remote(&self, remote: &Remote, updated: &mut HashSet<PathBuf>) -> Result<(), Error> {
    let path = self.clone_dir.join(remote.folder_name());
    if path.is_dir() && !updated.contains(&path) {
      updated.insert(path.clone());
      remote::checkout(&path, &remote.ref_)?;
      self.open_remote(remote)?.update_all_track(updated)?;
    }

    Ok(())
  }

  /// Recursively fetch/checkout for all modules that have already been cloned
  pub fn update_all(&self) -> Result<(), Error> {
    self.update_all_track(&mut HashSet::new())
  }

  /// Recursively fetch/checkout for all modules that have already been cloned,
  /// but with extra checks to avoid infinite recursion cycles
  fn update_all_track(&self, updated: &mut HashSet<PathBuf>) -> Result<(), Error> {
    // `updated` contains all of the directories that have been, well, updated.
    // it *needs* to be passed to recursive calls.

    // find all Includes that have already been cloned and update them
    for include in &self.data.includes {
      self.update_remote(&include.remote, updated)?;
    }

    Ok(())
  }

  /// Delete all cloned top-level targets
  pub fn clean_all(&self) -> Result<(), Error> {
    // no point in checking if it exists, because Mold::open creates it
    fs::remove_dir_all(&self.clone_dir)?;
    println!("{:>12} {}", "Deleted".red(), self.clone_dir.display());

    fs::remove_dir_all(&self.script_dir)?;
    println!("{:>12} {}", "Deleted".red(), self.script_dir.display());
    Ok(())
  }

  /// Merge every Include'd Mold into `self`
  pub fn process_includes(&mut self) -> Result<(), Error> {
    // merge all Includes into the current Mold. everything needs to be stuffed
    // into a vector because merging is a mutating action and `self` can't be
    // mutated while iterating through one of its fields.
    let mut others = vec![];
    for include in &self.data.includes {
      let path = self.clone_dir.join(include.remote.folder_name());
      let mut other = Self::discover(&path, include.remote.file.clone())?.adopt(self);

      // recursively merge
      other.process_includes()?;
      others.push((other, include.prefix.clone()));
    }

    for (data, prefix) in others {
      self.data.merge(data, &prefix);
    }

    Ok(())
  }

  /// Adopt any attributes from the parent that should be shared
  fn adopt(mut self, parent: &Self) -> Self {
    self.clone_dir = parent.clone_dir.clone();
    self.script_dir = parent.script_dir.clone();
    self.envs = parent.envs.clone();
    self
  }
}

// Help
impl Mold {
  /// Print a description of all recipes in this moldfile
  pub fn help(&self) -> Result<(), Error> {
    self.help_prefixed("")
  }

  /// Print a description of all recipes in this moldfile
  fn help_prefixed(&self, prefix: &str) -> Result<(), Error> {
    for (name, recipe) in &self.data.recipes {
      let colored_name = name.cyan();

      // this is supposed to be 12 character padded, but after all the
      // formatting, we end up with a String instead of a
      // colored::ColoredString, so we can't get the padding correct.  but I'm
      // pretty sure that all the color formatting just adds 18 non-display
      // characters, so padding to 30 works out?
      let display_name: String = format!("{}{}", prefix.magenta(), colored_name);
      println!("{:>30} {}", display_name, recipe.help());

      // print dependencies
      let deps = recipe.deps();
      if !deps.is_empty() {
        println!(
          "             ⮡ {}",
          deps
            .iter()
            .map(|x| format!("{}{}", prefix, x))
            .collect::<Vec<_>>()
            .join(" ")
            .cyan()
        );
      }
    }

    Ok(())
  }

  /// Print an explanation of global settings for this Moldfile
  pub fn explain_self(&self) -> Result<(), Error> {
    println!("{:12} {}", "environments:".white(), self.envs.join(" "));
    println!("{:12}", "conditionals:".white());

    let active = active_envs(&self.data.environments, &self.envs);

    for (cond, map) in &self.data.environments {
      let cond_disp = if active.contains(cond) {
        cond.green()
      } else {
        cond.blue()
      };

      println!("  {}:", cond_disp);
      for (key, val) in map {
        println!("    {:16} = {}", format!("${}", key).bright_cyan(), val);
      }
    }

    let vars = self.env_vars();
    println!("{:12}", "variables:".white());

    for (key, val) in &vars {
      println!("  {:16} = {}", format!("${}", key).bright_cyan(), val);
    }

    println!();

    Ok(())
  }

  /// Print an explanation of what a recipe does
  pub fn explain(&self, target_name: &str) -> Result<(), Error> {
    let target = self.find_recipe(target_name)?;

    println!("{:12}", target_name.cyan());
    if !target.help().is_empty() {
      println!("{:12} {}", "help:".white(), target.help());
    }

    if !target.vars_help().is_empty() {
      println!("{:12}", "variables:".white());

      for (name, desc) in target.vars_help() {
        println!(
          "  {}{} {}",
          format!("${}", name).bright_cyan(),
          ":".white(),
          desc
        );
      }
    }

    if !target.deps().is_empty() {
      println!(
        "{:12} {}",
        "depends on:".white(),
        target.deps().join(" ").cyan()
      );
    }

    if let Some(dir) = target.work_dir() {
      println!(
        "{:12} {}",
        "working dir:".white(),
        dir.display().to_string().cyan()
      );
    }

    println!("{:12} {}", "command:".white(), target.shell.to_string());

    if let Some(args) = self.build_args(target_name)? {
      println!(
        "{:12} {} {}",
        "executes:".white(),
        "$".green(),
        args.join(" ")
      );
    }

    // display contents of script file
    if let Some(script) = self.script_name(target_name)? {
      util::cat(script)?;
    }

    println!();

    Ok(())
  }
}

impl Moldfile {
  /// Merges any recipes from `other` that aren't in `self`
  pub fn merge(&mut self, other: Mold, prefix: &str) {
    for (recipe_name, recipe) in other.data.recipes {
      let mut new_recipe = recipe.clone();
      new_recipe.deps = new_recipe
        .deps
        .iter()
        .map(|x| format!("{}{}", prefix, x))
        .collect();

      self
        .recipes
        .entry(format!("{}{}", prefix, recipe_name))
        .or_insert(new_recipe);
    }
  }
}

impl Remote {
  /// Return this module's folder name in the format hash(url@ref)
  fn folder_name(&self) -> String {
    util::hash_url_ref(&self.url, &self.ref_)
  }
}

impl ToString for Remote {
  fn to_string(&self) -> String {
    if let Some(file) = &self.file {
      format!("{} @ {} /{}", self.url, self.ref_, file.display())
    } else {
      format!("{} @ {}", self.url, self.ref_)
    }
  }
}

impl Recipe {
  /// Return this recipe's dependencies
  fn deps(&self) -> Vec<String> {
    self.deps.clone()
  }

  /// Return this recipe's help string
  fn help(&self) -> &str {
    &self.help
  }

  /// Return this recipe's variables
  fn vars_help(&self) -> &VarMap {
    &self.variables
  }

  /// Return this recipe's working directory
  fn work_dir(&self) -> &Option<PathBuf> {
    &self.work_dir
  }
}
