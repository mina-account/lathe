use anyhow::{Context, Result, bail};
use quote::quote;
use std::{env, fs, path::PathBuf};
use syn::{ForeignItem, Item};

fn main() -> Result<()> {
    println!("cargo::rerun-if-changed=bindings");
    println!("cargo::rerun-if-changed=build.rs");
    let target = env::var("TARGET")?;
    let source = PathBuf::from("bindings").join(format!("{}.rs", target.replace('-', "_")));
    let parsed = syn::parse_file(
        &fs::read_to_string(&source).with_context(|| source.display().to_string())?,
    )?;
    let mut declarations = Vec::new();
    let mut fields = Vec::new();
    let mut loads = Vec::new();
    let mut wrappers = Vec::new();
    for item in parsed.items {
        let Item::ForeignMod(block) = item else {
            declarations.push(quote!(#item));
            continue;
        };
        if block.abi.name.as_ref().map(|name| name.value()).as_deref() != Some("C") {
            bail!("Unsupported CEF foreign ABI in {}", source.display());
        }
        for declaration in block.items {
            let ForeignItem::Fn(function) = declaration else {
                bail!(
                    "Unsupported CEF foreign declaration in {}",
                    source.display()
                );
            };
            let signature = &function.sig;
            if signature.variadic.is_some() || !signature.generics.params.is_empty() {
                bail!("Unsupported CEF function signature: {}", signature.ident);
            }
            let name = &signature.ident;
            let inputs = &signature.inputs;
            let output = &signature.output;
            let mut arguments = Vec::new();
            for argument in inputs {
                let syn::FnArg::Typed(argument) = argument else {
                    bail!("CEF foreign method cannot have a receiver");
                };
                let syn::Pat::Ident(pattern) = argument.pat.as_ref() else {
                    bail!("CEF function arguments must be identifiers");
                };
                arguments.push(pattern.ident.clone());
            }
            fields.push(quote!(#name: unsafe extern "C" fn(#inputs) #output));
            let symbol = format!("{name}\0").into_bytes();
            let symbol = syn::LitByteStr::new(&symbol, name.span());
            loads.push(quote!(#name: unsafe {
                *library.get(#symbol).map_err(|error| format!("Missing required CEF symbol {}: {error}", stringify!(#name)))?
            }));
            let attributes = &function.attrs;
            wrappers.push(quote! {
                #(#attributes)*
                pub unsafe extern "C" fn #name(#inputs) #output {
                    unsafe { (crate::loaded_api().#name)(#(#arguments),*) }
                }
            });
        }
    }
    if fields.is_empty() {
        bail!("CEF bindings contain no entrypoints");
    }
    let generated = quote! {
        #(#declarations)*
        pub(crate) struct Api {
            #(#fields,)*
            pub(crate) library: libloading::Library,
            pub(crate) path: std::path::PathBuf,
        }
        impl Api {
            pub(crate) unsafe fn load(path: &std::path::Path) -> Result<Self, String> {
                let library = unsafe { crate::open_library(path) }.map_err(|error| error.to_string())?;
                Ok(Self { #(#loads,)* library, path: path.to_owned() })
            }
        }
        #(#wrappers)*
    };
    fs::write(
        PathBuf::from(env::var("OUT_DIR")?).join("bindings.rs"),
        generated.to_string(),
    )?;
    if !cfg!(feature = "dox") {
        provision_runtime(&target)?;
    }
    Ok(())
}

fn provision_runtime(target: &str) -> Result<()> {
    use download_cef::OsAndArch;
    println!("cargo::rerun-if-env-changed=CEF_PATH");
    println!("cargo::rerun-if-env-changed=FLATPAK");
    println!("cargo::rerun-if-env-changed=NIX_CEF_BINARY");
    let platform = OsAndArch::try_from(target)?;
    let package_version = env::var("CARGO_PKG_VERSION")?;
    let cef_version = download_cef::default_version(&package_version);
    let output = PathBuf::from(env::var("OUT_DIR")?);
    if env::var_os("FLATPAK").is_some() {
        download_cef::check_archive_json(&package_version, "/usr/lib")?;
    }
    let configured = env::var_os("FLATPAK")
        .map(|_| PathBuf::from("/usr/lib"))
        .or_else(|| env::var_os("CEF_PATH").map(PathBuf::from));
    let runtime = if let Some(configured) = configured.as_ref().filter(|path| {
        download_cef::check_archive_json(&package_version, &path.to_string_lossy()).is_ok()
    }) {
        configured.clone()
    } else {
        let location = configured
            .map(|path| path.join(&cef_version))
            .unwrap_or_else(|| output.clone());
        let runtime = location.join(platform.to_string());
        if !runtime.exists() {
            if env::var_os("NIX_CEF_BINARY").is_some() {
                download_cef::install_nix_cef(&cef_version, &runtime, false)?;
            } else {
                let url = download_cef::default_download_url();
                let index = download_cef::CefIndex::download_from(&url)?;
                let version = index.platform(target)?.version(&cef_version)?;
                let archive = version.download_archive_from(&url, &location, false)?;
                let extracted =
                    download_cef::extract_target_archive(target, &archive, &location, false)?;
                if extracted.canonicalize()? != runtime.canonicalize()? {
                    bail!("Extracted CEF runtime does not match {}", runtime.display());
                }
                version.write_archive_json(&extracted)?;
            }
        }
        runtime
    };
    download_cef::check_archive_json(&package_version, &runtime.to_string_lossy())?;
    println!("cargo::rerun-if-changed={}", runtime.display());
    println!("cargo::metadata=CEF_DIR={}", runtime.display());
    // Cargo places build output three levels beneath the profile's binary folder.
    let destination = output
        .ancestors()
        .nth(3)
        .context("Cargo binary folder unavailable")?;
    for (source, destination) in [
        (runtime.clone(), destination.to_path_buf()),
        (runtime.join("locales"), destination.join("locales")),
    ] {
        fs::create_dir_all(&destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::copy(entry.path(), destination.join(entry.file_name()))?;
            }
        }
    }
    Ok(())
}
