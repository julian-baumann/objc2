use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use apple_sdk::{AppleSdk, DeveloperDirectory, Platform, SdkPath, SimpleSdk};
use clang::{Clang, Entity, EntityKind, EntityVisitResult, Index, TranslationUnit};

use header_translator::{compare_btree, run_cargo_fmt, Config, File, Library, Stmt};

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = manifest_dir.parent().unwrap();
    let crate_src = workspace_dir.join("icrate/src");

    println!("status: loading configs...");
    let configs = load_configs(&crate_src);
    println!("status: loaded {} configs", configs.len());

    let clang = Clang::new().unwrap();
    let index = Index::new(&clang, true, true);

    let developer_dir = DeveloperDirectory::from(PathBuf::from(
        std::env::args_os()
            .skip(1)
            .next()
            .expect("must specify developer directory as first argument"),
    ));

    let sdks: Vec<_> = developer_dir
        .platforms()
        .expect("developer dir platforms")
        .into_iter()
        .map(|platform| {
            let sdks: Vec<_> = platform
                .find_sdks::<SimpleSdk>()
                .expect("platform sdks")
                .into_iter()
                .filter(|sdk| !sdk.is_symlink() && sdk.platform() == &*platform)
                .collect();
            if sdks.len() != 1 {
                panic!("found multiple sdks {sdks:?} in {:?}", &*platform);
            }
            sdks[0].sdk_path()
        })
        .collect();

    assert_eq!(sdks.len(), 8, "should have one of each platform: {sdks:?}");

    let mut final_result = None;

    // TODO: Compare between SDKs
    for sdk in sdks {
        println!("status: parsing {:?}...", sdk.platform);
        // These are found using the `get_llvm_targets.fish` helper script
        let llvm_targets: &[_] = match &sdk.platform {
            Platform::MacOsX => &[
                "x86_64-apple-macosx10.7.0",
                // "arm64-apple-macosx11.0.0",
                // "i686-apple-macosx10.7.0",
            ],
            Platform::IPhoneOs => &[
                // "arm64-apple-ios7.0.0",
                // "armv7-apple-ios7.0.0",
                // "armv7s-apple-ios",
                // "arm64-apple-ios14.0-macabi",
                // "x86_64-apple-ios13.0-macabi",
            ],
            // Platform::IPhoneSimulator => &[
            //     "arm64-apple-ios7.0.0-simulator",
            //     "x86_64-apple-ios7.0.0-simulator",
            //     "i386-apple-ios7.0.0-simulator",
            // ],
            // Platform::AppleTvOs => &["arm64-apple-tvos", "x86_64-apple-tvos"],
            // Platform::WatchOs => &["arm64_32-apple-watchos", "armv7k-apple-watchos"],
            // Platform::WatchSimulator => &[
            //     "arm64-apple-watchos5.0.0-simulator",
            //     "x86_64-apple-watchos5.0.0-simulator",
            // ],
            _ => continue,
        };

        let mut result = None;

        for llvm_target in llvm_targets {
            println!("status:     parsing llvm target {llvm_target:?}...");
            let curr_result = parse_sdk(&index, &sdk, llvm_target, &configs);
            println!("status:     done parsing llvm target {llvm_target:?}");

            if let Some(prev_result) = &result {
                compare_results(prev_result, &curr_result);
            } else {
                result = Some(curr_result);
            }
        }

        if sdk.platform == Platform::MacOsX {
            final_result = result;
        }
        println!("status: done parsing {:?}", sdk.platform);
    }

    for (library, files) in final_result.expect("got a result") {
        println!("status: writing framework {library}...");
        let output_path = crate_src.join("generated").join(&library);
        files.output(&output_path).unwrap();
        println!("status: written framework {library}");
    }

    println!("status: formatting");
    run_cargo_fmt("icrate");
}

fn load_configs(crate_src: &Path) -> BTreeMap<String, Config> {
    crate_src
        .read_dir()
        .expect("read_dir")
        .filter_map(|dir| {
            let dir = dir.expect("dir");
            if !dir.file_type().expect("file type").is_dir() {
                return None;
            }
            let path = dir.path();
            let file = path.join("translation-config.toml");
            match Config::from_file(&file) {
                Ok(config) => Some((
                    path.file_name()
                        .expect("framework name")
                        .to_string_lossy()
                        .to_string(),
                    config,
                )),
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(err) => panic!("{file:?}: {err}"),
            }
        })
        .collect()
}

fn parse_sdk(
    index: &Index<'_>,
    sdk: &SdkPath,
    llvm_target: &str,
    configs: &BTreeMap<String, Config>,
) -> BTreeMap<String, Library> {
    let tu = get_translation_unit(index, sdk, llvm_target);
    println!("status: initialized translation unit {:?}", sdk.platform);

    let framework_dir = sdk.path.join("System/Library/Frameworks");

    let mut preprocessing = true;
    let mut result: BTreeMap<_, _> = configs
        .iter()
        .map(|(name, _)| (name.clone(), Library::new()))
        .collect();

    tu.get_entity().visit_children(|entity, _parent| {
        if let Some((library_name, file_name)) = extract_framework_name(&entity, &framework_dir) {
            if let Some(config) = configs.get(&library_name) {
                let library = result.get_mut(&library_name).expect("library");
                match entity.get_kind() {
                    EntityKind::InclusionDirective if preprocessing => {
                        // println!("{library_name}/{file_name}.h: {entity:?}");
                        // If umbrella header
                        let name = entity.get_name().expect("inclusion name");
                        let mut iter = name.split('/');
                        let framework = iter.next().expect("inclusion name has framework");
                        if framework == library_name {
                            let included = iter
                                .next()
                                .expect("inclusion name has file")
                                .strip_suffix(".h")
                                .expect("inclusion name file is header")
                                .to_string();
                            if iter.count() != 0 {
                                panic!("invalid inclusion of {name:?}");
                            }

                            // If inclusion is not umbrella header
                            if included != library_name {
                                // The file is often included twice, even
                                // within the same file, so insertion can fail
                                library
                                    .files
                                    .entry(included)
                                    .or_insert_with(|| File::new(&config));
                            }
                        }
                    }
                    EntityKind::MacroExpansion if preprocessing => {}
                    EntityKind::MacroDefinition if preprocessing => {
                        // let name = entity.get_name().expect("macro def name");
                        // entity.is_function_like_macro();
                        // println!("macrodef in {library_name}/{file_name}.h: {}", name);
                    }
                    _ => {
                        if preprocessing {
                            println!("status: preprocessed {:?}...", sdk.platform);
                        }
                        preprocessing = false;
                        // No more includes / macro expansions after this line
                        let file = library.files.get_mut(&file_name).expect("file");
                        for stmt in Stmt::parse(&entity, &config) {
                            file.add_stmt(stmt);
                        }
                    }
                }
            } else {
                // println!("library not found {library_name}");
            }
        }
        EntityVisitResult::Continue
    });

    result
}

fn get_translation_unit<'i: 'tu, 'tu>(
    index: &'i Index<'tu>,
    sdk: &SdkPath,
    llvm_target: &str,
) -> TranslationUnit<'tu> {
    let target = format!("--target={llvm_target}");

    let tu = index
        .parser(&Path::new(env!("CARGO_MANIFEST_DIR")).join("framework-includes.h"))
        .detailed_preprocessing_record(true)
        .incomplete(true)
        .skip_function_bodies(true)
        .keep_going(true)
        // .single_file_parse(true)
        .include_attributed_types(true)
        .visit_implicit_attributes(true)
        // .ignore_non_errors_from_included_files(true)
        .retain_excluded_conditional_blocks(true)
        .arguments(&[
            "-x",
            "objective-c",
            &target,
            "-Wall",
            "-Wextra",
            "-fobjc-arc",
            "-fobjc-arc-exceptions",
            "-fobjc-abi-version=2", // 3??
            // "-fparse-all-comments",
            "-fapinotes",
            "-isysroot",
            sdk.path.to_str().unwrap(),
        ])
        .parse()
        .unwrap();

    // dbg!(&tu);
    // dbg!(tu.get_target());
    // dbg!(tu.get_memory_usage());
    // dbg!(tu.get_diagnostics());

    // let dbg_file = |file: File<'_>| {
    //     dbg!(
    //         &file,
    //         file.get_module(),
    //         file.get_skipped_ranges(),
    //         file.is_include_guarded(),
    //         // file.get_includes(),
    //         // file.get_references(),
    //     );
    // };
    //
    // dbg_file(tu.get_file(&header).unwrap());
    // dbg_file(tu.get_file(&dir.join("NSAccessibility.h")).unwrap());
    // let cursor_file = tu.get_file(&dir.join("NSCursor.h")).unwrap();
    // dbg_file(cursor_file);

    tu
}

pub fn extract_framework_name(
    entity: &Entity<'_>,
    framework_dir: &Path,
) -> Option<(String, String)> {
    if let Some(location) = entity.get_location() {
        if let Some(file) = location.get_file_location().file {
            let path = file.get_path();
            if let Ok(path) = path.strip_prefix(&framework_dir) {
                let mut components = path.components();
                let library_name = components
                    .next()
                    .expect("components next")
                    .as_os_str()
                    .to_str()
                    .expect("component to_str")
                    .strip_suffix(".framework")
                    .expect("framework fileending")
                    .to_string();

                let path = components.as_path();
                let file_name = path
                    .file_stem()
                    .expect("path file stem")
                    .to_string_lossy()
                    .to_string();

                return Some((library_name, file_name));
            }
        }
    }
    None
}

fn compare_results(data1: &BTreeMap<String, Library>, data2: &BTreeMap<String, Library>) {
    compare_btree(data1, data2, |libary_name, library1, library2| {
        println!("comparing {libary_name}");
        library1.compare(library2);
    });

    // Extra check in case our comparison above was not exaustive
    assert_eq!(data1, data2);
}
