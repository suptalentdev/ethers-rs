#![deny(missing_docs, unsafe_code)]

//! Module for generating type-safe bindings to Ethereum smart contracts. This
//! module is intended to be used either indirectly with the `abigen` procedural
//! macro or directly from a build script / CLI

#[cfg(test)]
#[allow(missing_docs)]
#[macro_use]
#[path = "test/macros.rs"]
mod test_macros;

/// Contains types to generate rust bindings for solidity contracts
pub mod contract;
use contract::Context;

pub mod rawabi;
mod rustfmt;
mod source;
mod util;

pub use ethers_core::types::Address;
pub use source::Source;
pub use util::parse_address;

use anyhow::Result;
use inflector::Inflector;
use proc_macro2::TokenStream;
use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write,
    path::Path,
};

/// Builder struct for generating type-safe bindings from a contract's ABI
///
/// Note: Your contract's ABI must contain the `stateMutability` field. This is
/// [still not supported by Vyper](https://github.com/vyperlang/vyper/issues/1931), so you must adjust your ABIs and replace
/// `constant` functions with `view` or `pure`.
///
/// # Example
///
/// Running the command below will generate a file called `token.rs` containing the
/// bindings inside, which exports an `ERC20Token` struct, along with all its events.
///
/// ```no_run
/// # use ethers_contract_abigen::Abigen;
/// # fn foo() -> Result<(), Box<dyn std::error::Error>> {
/// Abigen::new("ERC20Token", "./abi.json")?.generate()?.write_to_file("token.rs")?;
/// # Ok(())
/// # }
#[derive(Debug, Clone)]
pub struct Abigen {
    /// The source of the ABI JSON for the contract whose bindings
    /// are being generated.
    abi_source: Source,

    /// Override the contract name to use for the generated type.
    contract_name: String,

    /// Manually specified contract method aliases.
    method_aliases: HashMap<String, String>,

    /// Derives added to event structs and enums.
    event_derives: Vec<String>,

    /// Format the code using a locally installed copy of `rustfmt`.
    rustfmt: bool,

    /// Manually specified event name aliases.
    event_aliases: HashMap<String, String>,
}

impl Abigen {
    /// Creates a new builder with the given ABI JSON source.
    pub fn new<S: AsRef<str>>(contract_name: &str, abi_source: S) -> Result<Self> {
        let abi_source = abi_source.as_ref().parse()?;
        Ok(Self {
            abi_source,
            contract_name: contract_name.to_owned(),
            method_aliases: HashMap::new(),
            event_derives: Vec::new(),
            event_aliases: HashMap::new(),
            rustfmt: true,
        })
    }

    /// Manually adds a solidity event alias to specify what the event struct
    /// and function name will be in Rust.
    #[must_use]
    pub fn add_event_alias<S1, S2>(mut self, signature: S1, alias: S2) -> Self
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        self.event_aliases.insert(signature.into(), alias.into());
        self
    }

    /// Manually adds a solidity method alias to specify what the method name
    /// will be in Rust. For solidity methods without an alias, the snake cased
    /// method name will be used.
    #[must_use]
    pub fn add_method_alias<S1, S2>(mut self, signature: S1, alias: S2) -> Self
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        self.method_aliases.insert(signature.into(), alias.into());
        self
    }

    /// Specify whether or not to format the code using a locally installed copy
    /// of `rustfmt`.
    ///
    /// Note that in case `rustfmt` does not exist or produces an error, the
    /// unformatted code will be used.
    #[must_use]
    pub fn rustfmt(mut self, rustfmt: bool) -> Self {
        self.rustfmt = rustfmt;
        self
    }

    /// Add a custom derive to the derives for event structs and enums.
    ///
    /// This makes it possible to for example derive serde::Serialize and
    /// serde::Deserialize for events.
    #[must_use]
    pub fn add_event_derive<S>(mut self, derive: S) -> Self
    where
        S: Into<String>,
    {
        self.event_derives.push(derive.into());
        self
    }

    /// Generates the contract bindings.
    pub fn generate(self) -> Result<ContractBindings> {
        let rustfmt = self.rustfmt;
        let tokens = Context::from_abigen(self)?.expand()?.into_tokens();
        Ok(ContractBindings { tokens, rustfmt })
    }
}

/// Type-safe contract bindings generated by a `Builder`. This type can be
/// either written to file or into a token stream for use in a procedural macro.
pub struct ContractBindings {
    /// The TokenStream representing the contract bindings.
    tokens: TokenStream,
    /// The output options used for serialization.
    rustfmt: bool,
}

impl ContractBindings {
    /// Writes the bindings to a given `Write`.
    pub fn write<W>(&self, mut w: W) -> Result<()>
    where
        W: Write,
    {
        let source = {
            let raw = self.tokens.to_string();

            if self.rustfmt {
                rustfmt::format(&raw).unwrap_or(raw)
            } else {
                raw
            }
        };

        w.write_all(source.as_bytes())?;
        Ok(())
    }

    /// Writes the bindings to the specified file.
    pub fn write_to_file<P>(&self, path: P) -> Result<()>
    where
        P: AsRef<Path>,
    {
        let file = File::create(path)?;
        self.write(file)
    }

    /// Converts the bindings into its underlying token stream. This allows it
    /// to be used within a procedural macro.
    pub fn into_tokens(self) -> TokenStream {
        self.tokens
    }
}

/// Generates bindings for a series of contracts
///
/// This type can be used to generate multiple `ContractBindings` and put them all in a single rust
/// module, (eg. a `contracts` directory).
///
/// This can be used to
/// 1) write all bindings directly into a new directory in the project's source directory, so that
/// it is included in the repository. 2) write all bindings to the value of cargo's `OUT_DIR` in a
/// build script and import the bindings as `include!(concat!(env!("OUT_DIR"), "/mod.rs"));`.
///
/// However, the main purpose of this generator is to create bindings for option `1)` and write all
/// contracts to some `contracts`  module in `src`, like `src/contracts/mod.rs` __once__ via a build
/// script or a test. After that it's recommend to remove the build script and replace it with an
/// integration test (See `MultiAbigen::ensure_consistent_bindings`) that fails if the generated
/// code is out of date. This has several advantages:
///
///   * No need for downstream users to compile the build script
///   * No need for downstream users to run the whole `abigen!` generation steps
///   * The generated code is more usable in an IDE
///   * CI will fail if the generated code is out of date (if `abigen!` or the contract's ABI itself
///     changed)
///
/// See `MultiAbigen::ensure_consistent_bindings` for the recommended way to set this up to generate
/// the bindings once via a test and then use the test to ensure consistency.
#[derive(Debug, Clone)]
pub struct MultiAbigen {
    /// whether to write all contracts in a single file instead of separated modules
    single_file: bool,

    abigens: Vec<Abigen>,
}

impl MultiAbigen {
    /// Create a new instance from a series of already resolved `Abigen`
    pub fn from_abigen(abis: impl IntoIterator<Item = Abigen>) -> Self {
        Self {
            single_file: false,
            abigens: abis.into_iter().map(|abi| abi.rustfmt(true)).collect(),
        }
    }

    /// Create a new instance from a series (`contract name`, `abi_source`)
    ///
    /// See `Abigen::new`
    pub fn new<I, Name, Source>(abis: I) -> Result<Self>
    where
        I: IntoIterator<Item = (Name, Source)>,
        Name: AsRef<str>,
        Source: AsRef<str>,
    {
        let abis = abis
            .into_iter()
            .map(|(contract_name, abi_source)| Abigen::new(contract_name.as_ref(), abi_source))
            .collect::<Result<Vec<_>>>()?;

        Ok(Self::from_abigen(abis))
    }

    /// Reads all json files contained in the given `dir` and use the file name for the name of the
    /// `ContractBindings`.
    /// This is equivalent to calling `MultiAbigen::new` with all the json files and their filename.
    ///
    /// # Example
    ///
    /// ```text
    /// abi
    /// ├── ERC20.json
    /// ├── Contract1.json
    /// ├── Contract2.json
    /// ...
    /// ```
    ///
    /// ```no_run
    /// # use ethers_contract_abigen::MultiAbigen;
    /// let gen = MultiAbigen::from_json_files("./abi").unwrap();
    /// ```
    pub fn from_json_files(dir: impl AsRef<Path>) -> Result<Self> {
        let mut abis = Vec::new();
        for file in util::json_files(dir) {
            if let Some(file_name) = file.file_stem().and_then(|s| s.to_str()) {
                let content = fs::read_to_string(&file)?;
                abis.push((file_name.to_string(), content));
            }
        }
        Self::new(abis)
    }

    /// Write all bindings into a single rust file instead of separate modules
    #[must_use]
    pub fn single_file(mut self) -> Self {
        self.single_file = true;
        self
    }

    /// Generates all the bindings and writes them to the given module
    ///
    /// # Example
    ///
    /// Read all json abi files from the `./abi` directory
    /// ```text
    /// abi
    /// ├── ERC20.json
    /// ├── Contract1.json
    /// ├── Contract2.json
    /// ...
    /// ```
    ///
    /// and write them to the `./src/contracts` location as
    ///
    /// ```text
    /// src/contracts
    /// ├── mod.rs
    /// ├── er20.rs
    /// ├── contract1.rs
    /// ├── contract2.rs
    /// ...
    /// ```
    ///
    /// ```no_run
    /// # use ethers_contract_abigen::MultiAbigen;
    /// let gen = MultiAbigen::from_json_files("./abi").unwrap();
    /// gen.write_to_module("./src/contracts").unwrap();
    /// ```
    pub fn write_to_module(self, module: impl AsRef<Path>) -> Result<()> {
        let module = module.as_ref();
        fs::create_dir_all(module)?;

        let mut contracts_mod =
            b"/// This module contains all the autogenerated abigen! contract bindings\n".to_vec();

        let mut modules = Vec::new();
        for abi in self.abigens {
            let name = abi.contract_name.to_snake_case();
            let bindings = abi.generate()?;
            if self.single_file {
                // append to the mod file
                bindings.write(&mut contracts_mod)?;
            } else {
                // create a contract rust file
                let output = module.join(format!("{}.rs", name));
                bindings.write_to_file(output)?;
                modules.push(format!("pub mod {};", name));
            }
        }

        if !modules.is_empty() {
            modules.sort();
            write!(contracts_mod, "{}", modules.join("\n"))?;
        }

        // write the mod file
        fs::write(module.join("mod.rs"), contracts_mod)?;

        Ok(())
    }

    /// This ensures that the already generated contract bindings match the output of a fresh new
    /// run. Run this in a rust test, to get notified in CI if the newly generated bindings
    /// deviate from the already generated ones, and it's time to generate them again. This could
    /// happen if the ABI of a contract or the output that `ethers` generates changed.
    ///
    /// So if this functions is run within a test during CI and fails, then it's time to update all
    /// bindings.
    ///
    /// Returns `true` if the freshly generated bindings match with the existing bindings, `false`
    /// otherwise
    ///
    /// # Example
    ///
    /// Check that the generated files are up to date
    ///
    /// ```no_run
    /// # use ethers_contract_abigen::MultiAbigen;
    /// #[test]
    /// fn generated_bindings_are_fresh() {
    ///  let project_root = std::path::Path::new(&env!("CARGO_MANIFEST_DIR"));
    ///  let abi_dir = project_root.join("abi");
    ///  let gen = MultiAbigen::from_json_files(&abi_dir).unwrap();
    ///  assert!(gen.ensure_consistent_bindings(project_root.join("src/contracts")));
    /// }
    ///
    /// gen.write_to_module("./src/contracts").unwrap();
    /// ```
    #[cfg(test)]
    pub fn ensure_consistent_bindings(self, module: impl AsRef<Path>) -> bool {
        let module = module.as_ref();
        let dir = tempfile::tempdir().expect("Failed to create temp dir");
        let temp_module = dir.path().join("contracts");
        self.write_to_module(&temp_module).expect("Failed to generate bindings");

        for file in fs::read_dir(&temp_module).unwrap() {
            let fresh_file = file.unwrap();
            let fresh_file_path = fresh_file.path();
            let file_name = fresh_file_path.file_name().and_then(|p| p.to_str()).unwrap();
            assert!(file_name.ends_with(".rs"), "Expected rust file");

            let existing_bindings_file = module.join(file_name);

            if !existing_bindings_file.is_file() {
                // file does not already exist
                return false
            }

            // read the existing file
            let existing_contract_bindings = fs::read_to_string(existing_bindings_file).unwrap();

            let fresh_bindings = fs::read_to_string(fresh_file.path()).unwrap();

            if existing_contract_bindings != fresh_bindings {
                return false
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_generate_multi_abi() {
        let crate_root = std::path::Path::new(&env!("CARGO_MANIFEST_DIR"));

        let tempdir = tempfile::tempdir().unwrap();
        let mod_root = tempdir.path().join("contracts");

        let console = Abigen::new(
            "Console",
            crate_root.join("../tests/solidity-contracts/console.json").display().to_string(),
        )
        .unwrap();

        let simple_storage = Abigen::new(
            "SimpleStorage",
            crate_root
                .join("../tests/solidity-contracts/simplestorage_abi.json")
                .display()
                .to_string(),
        )
        .unwrap();

        let human_readable = Abigen::new(
            "HrContract",
            r#"[
        struct Foo { uint256 x; }
        function foo(Foo memory x)
        function bar(uint256 x, uint256 y, address addr)
        yeet(uint256,uint256,address)
    ]"#,
        )
        .unwrap();

        let mut multi_gen = MultiAbigen::from_abigen([console, simple_storage, human_readable]);

        multi_gen.clone().write_to_module(&mod_root).unwrap();
        assert!(multi_gen.clone().ensure_consistent_bindings(&mod_root));

        // add another contract
        multi_gen.abigens.push(
            Abigen::new(
                "AdditionalContract",
                r#"[
        getValue() (uint256)
        getValue(uint256 otherValue) (uint256)
        getValue(uint256 otherValue, address addr) (uint256)
    ]"#,
            )
            .unwrap(),
        );

        // ensure inconsistent bindings are detected
        assert!(!multi_gen.clone().ensure_consistent_bindings(&mod_root));

        // update with new contract
        multi_gen.clone().write_to_module(&mod_root).unwrap();
        assert!(multi_gen.clone().ensure_consistent_bindings(&mod_root));
    }
}
