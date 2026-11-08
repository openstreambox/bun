use super::super::*;
use super::MOST_STORES_FOLLOWED;
use crate::compile::testing::{default_settings, with_bind_input};
use crate::diagnostics::range_of_node;
use crate::ir::run_serially;
use crate::modules::bind_modules;
use bun_core::strings::count_char;
use bun_sema::hir::NodeData;

const FUNCTION: &str = "function () { return 1; }";

/// `<file>:<line> <hook>` for each store that a build points at.
fn find_hooks_replaced(files: &[(&str, &str)]) -> Vec<String> {
    with_bind_input(files, &default_settings(), |bind_input| {
        let graph = bind_modules(bind_input).graph;
        assert!(graph.errors().is_empty(), "the program links");
        let input = ProgramReachInput {
            files: bind_input.files,
            sources: bind_input.sources,
            graph: &graph,
            known_helpers: bind_input.known_helpers,
            builtin_names: bind_input.builtin_names,
            platform: bind_input.platform,
            removable: RemovableOptions::default(),
            counts_executions: false,
            any_file_is_named_at_run_time: false,
            files_are_named_out_of_sight: false,
            admits_inspector: false,
            isolates_code_from_disk: false,
            mocks_any_module: false,
            files_started_by_runner: &[],
            starts_of_workers: &[],
            beside: bind_input.beside,
        };
        let (_, reached) = find_left_out_code(&input, &run_serially);
        let describe = |replaced: &ReplacedLoaderHook| {
            let hir = input.files.hir(replaced.file);
            let name = input.files.module(replaced.file).file_name();
            let name = name.rsplit(|&byte| byte == b'/').next().expect("a name");
            let (start, _) = range_of_node(hir, NodeData::Expr(replaced.store));
            let line = 1 + count_char(&hir.text[..start as usize], b'\n');
            let (name, hook) = (name.to_vec(), replaced.hook.name().to_vec());
            let to_text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("a name in ASCII");
            format!("{}:{line} {}", to_text(name), to_text(hook))
        };
        reached.replaced_loader_hooks.iter().map(describe).collect()
    })
}

fn find_hooks_replaced_in(main: &str) -> Vec<String> {
    find_hooks_replaced(&[("main.cjs", main)])
}

#[test]
fn store_that_spells_the_hook_is_found_where_it_stands() {
    let main = format!(
        "const Module = require('node:module');\n\
         Module._resolveFilename = {FUNCTION};\n\
         require('node:module').prototype.require = {FUNCTION};\n\
         const same = Module;\n\
         same['_resolveFilename'] = {FUNCTION};"
    );
    assert_eq!(
        find_hooks_replaced_in(&main),
        [
            "main.cjs:2 Module._resolveFilename",
            "main.cjs:3 Module.prototype.require",
            "main.cjs:5 Module._resolveFilename",
        ]
    );
}

#[test]
fn store_through_a_choice_of_objects_is_found() {
    for holder in [
        "process.argv.length > 99 ? module.constructor : require('node:module')",
        "require('node:module') || null",
        "globalThis.noSuchModule ?? require('node:module')",
        "process.argv.length && require('node:module')",
        "(0, require('node:module'))",
    ] {
        let main = format!(
            "const Module = {holder};\n\
             Module._resolveFilename = {FUNCTION};\n\
             Module.prototype.require = {FUNCTION};"
        );
        assert_eq!(
            find_hooks_replaced_in(&main),
            [
                "main.cjs:2 Module._resolveFilename",
                "main.cjs:3 Module.prototype.require",
            ],
            "{holder}"
        );
    }
}

#[test]
fn store_through_a_variable_that_is_assigned_later_is_found() {
    let main = format!(
        "let Module;\n\
         try {{ Module = require('node:module'); }} catch {{ Module = null; }}\n\
         Module._resolveFilename = {FUNCTION};\n\
         let other = null;\n\
         other ??= require('node:module').prototype;\n\
         other.require = {FUNCTION};"
    );
    assert_eq!(
        find_hooks_replaced_in(&main),
        [
            "main.cjs:3 Module._resolveFilename",
            "main.cjs:6 Module.prototype.require",
        ]
    );
}

#[test]
fn variable_that_is_assigned_often_is_not_followed() {
    let stores = "Module = require('node:module');\n".repeat(MOST_STORES_FOLLOWED + 1);
    let main = format!("let Module;\n{stores}Module._resolveFilename = {FUNCTION};");
    assert!(find_hooks_replaced_in(&main).is_empty());
}

#[test]
fn store_inside_a_pattern_is_found_at_the_member() {
    let main = format!(
        "const Module = require('node:module');\n\
         let first;\n\
         [first,\n\
          Module._resolveFilename] = [1, {FUNCTION}];\n\
         ({{ a: first,\n\
            b: Module.prototype.require = {FUNCTION},\n\
            ...Module.prototype.rest }} = {{}});"
    );
    assert_eq!(
        find_hooks_replaced_in(&main),
        [
            "main.cjs:4 Module._resolveFilename",
            "main.cjs:6 Module.prototype.require",
        ]
    );
}

#[test]
fn store_by_a_call_that_copies_or_sets_is_found_at_the_call() {
    let main = format!(
        "const Module = require('node:module');\n\
         Object.assign(Module, {{ other: 1 }}, {{ _resolveFilename: {FUNCTION} }});\n\
         Object.assign(Module.prototype, {{ require() {{ return 1; }} }});\n\
         Reflect.set(Module, '_resolveFilename', {FUNCTION});\n\
         Reflect.set({{}}, 'require', {FUNCTION}, Module.prototype);\n\
         const key = '_resolve' + 'Filename';\n\
         Object.assign(Module, {{ [key]: {FUNCTION} }});"
    );
    assert_eq!(
        find_hooks_replaced_in(&main),
        [
            "main.cjs:2 Module._resolveFilename",
            "main.cjs:3 Module.prototype.require",
            "main.cjs:4 Module._resolveFilename",
            "main.cjs:5 Module.prototype.require",
            "main.cjs:7 Module._resolveFilename",
        ]
    );
}

#[test]
fn call_that_copies_or_sets_something_else_is_no_store_of_a_hook() {
    for call in [
        "Object.assign(Module, { _load: FUNCTION });",
        "Object.assign({}, { _resolveFilename: FUNCTION });",
        "Object.assign(Module, ...[{ _resolveFilename: FUNCTION }]);",
        "Object.assign(Module, patches);",
        "Reflect.set(Module, process.argv[2], FUNCTION);",
        "Reflect.set(Module, '_resolveFilename', FUNCTION, {});",
        "Object.defineProperty(Module, '_resolveFilename', { value: FUNCTION });",
        "Object.defineProperties(Module, { _resolveFilename: { value: FUNCTION } });",
        "Reflect.defineProperty(Module.prototype, 'require', { value: FUNCTION });",
    ] {
        let main = format!(
            "const Module = require('node:module');\n\
             const patches = {{ _resolveFilename: {FUNCTION} }};\n\
             {}",
            call.replace("FUNCTION", FUNCTION)
        );
        assert!(find_hooks_replaced_in(&main).is_empty(), "{call}");
    }
}

#[test]
fn store_through_a_parameter_is_found_in_the_function_that_is_handed_the_object() {
    for (hand_off, line) in [
        (
            "function install(Mod) {\n  Mod._resolveFilename = FUNCTION;\n}\ninstall(require('node:module'));",
            2,
        ),
        (
            "const install = Mod => {\n  Mod._resolveFilename = FUNCTION;\n};\ninstall(require('node:module'));",
            2,
        ),
        (
            "const helper = {\n  install(Mod) {\n    Mod._resolveFilename = FUNCTION;\n  },\n};\nhelper.install(require('node:module'));",
            3,
        ),
        (
            "(function (Mod) {\n  Mod._resolveFilename = FUNCTION;\n})(require('node:module'));",
            2,
        ),
        (
            "const Module = require('node:module');\nfunction install(first, Mod) {\n  const held = Mod;\n  Object.assign(held, { _resolveFilename: FUNCTION });\n}\ninstall(1, Module);",
            4,
        ),
    ] {
        let main = hand_off.replace("FUNCTION", FUNCTION);
        assert_eq!(
            find_hooks_replaced_in(&main),
            [format!("main.cjs:{line} Module._resolveFilename")],
            "{hand_off}"
        );
    }
}

#[test]
fn store_through_a_parameter_is_found_for_the_prototype_too() {
    let main = format!(
        "const Module = require('node:module');\n\
         function install(Mod, prototype) {{\n\
           Mod.prototype.require = {FUNCTION};\n\
           prototype.require = {FUNCTION};\n\
         }}\n\
         install(Module, Module.prototype);"
    );
    assert_eq!(
        find_hooks_replaced_in(&main),
        [
            "main.cjs:3 Module.prototype.require",
            "main.cjs:4 Module.prototype.require",
        ]
    );
}

#[test]
fn store_through_a_parameter_is_found_in_another_file() {
    let install = format!(
        "module.exports = function install(Mod, wrap) {{\n\
           Mod._resolveFilename = wrap(Mod._resolveFilename);\n\
         }};\n\
         module.exports.never = function (Mod) {{\n\
           Mod.prototype.require = {FUNCTION};\n\
         }};"
    );
    let files = [
        (
            "main.cjs",
            "require('./install.cjs')(require('node:module'), it => it);",
        ),
        ("install.cjs", install.as_str()),
    ];
    assert_eq!(
        find_hooks_replaced(&files),
        ["install.cjs:2 Module._resolveFilename"]
    );
}

#[test]
fn store_through_a_parameter_is_found_behind_an_import() {
    let install = format!(
        "export function install(Mod) {{\n\
           patch(Mod.prototype);\n\
         }}\n\
         function patch(prototype) {{\n\
           prototype.require = {FUNCTION};\n\
         }}"
    );
    let files = [
        (
            "main.mjs",
            "import Module from 'node:module';\nimport { install } from './install.mjs';\ninstall(Module);",
        ),
        ("install.mjs", install.as_str()),
    ];
    assert_eq!(
        find_hooks_replaced(&files),
        ["install.mjs:5 Module.prototype.require"]
    );
}

#[test]
fn function_that_hands_the_object_to_itself_is_walked_once() {
    let main = format!(
        "function install(Mod, depth) {{\n\
           if (depth) install(Mod, depth - 1);\n\
           Mod._resolveFilename = {FUNCTION};\n\
         }}\n\
         install(require('node:module'), 2);\n\
         install(require('node:module'), 3);"
    );
    assert_eq!(
        find_hooks_replaced_in(&main),
        ["main.cjs:3 Module._resolveFilename"]
    );
}

#[test]
fn store_that_the_hand_off_does_not_run_is_not_found() {
    for main in [
        "function install(Mod) {\n  return () => { Mod._resolveFilename = FUNCTION; };\n}\ninstall(require('node:module'));",
        "function install(Mod) {\n  function later() { Mod._resolveFilename = FUNCTION; }\n  return later;\n}\ninstall(require('node:module'));",
        "function install(Mod) {\n  Mod._resolveFilename = FUNCTION;\n}\nfunction never() {\n  install(require('node:module'));\n}",
        "function install(Mod) {\n  if (false) Mod._resolveFilename = FUNCTION;\n}\ninstall(require('node:module'));",
        "function* install(Mod) {\n  Mod._resolveFilename = FUNCTION;\n}\ninstall(require('node:module'));",
    ] {
        let hooks = find_hooks_replaced_in(&main.replace("FUNCTION", FUNCTION));
        assert!(hooks.is_empty(), "{main}: {hooks:?}");
    }
}

#[test]
fn parameter_that_may_hold_something_else_is_not_followed() {
    for main in [
        "function install(Mod) {\n  Mod = {};\n  Mod._resolveFilename = FUNCTION;\n}\ninstall(require('node:module'));",
        "function install(...rest) {\n  rest._resolveFilename = FUNCTION;\n}\ninstall(require('node:module'));",
        "function install({ Mod }) {\n  Mod._resolveFilename = FUNCTION;\n}\ninstall(require('node:module'));",
        "function install(first, Mod) {\n  Mod._resolveFilename = FUNCTION;\n}\ninstall(require('node:module'));",
        "function install(Mod) {\n  Mod._resolveFilename = FUNCTION;\n}\ninstall(...[require('node:module')]);",
        "function install(Mod) {\n  Mod._resolveFilename = FUNCTION;\n}\ninstall.call(null, require('node:module'));",
        "function install(Mod) {\n  Mod._resolveFilename = FUNCTION;\n}\ninstall({ Mod: require('node:module') });",
    ] {
        let hooks = find_hooks_replaced_in(&main.replace("FUNCTION", FUNCTION));
        assert!(hooks.is_empty(), "{main}: {hooks:?}");
    }
}

#[test]
fn store_to_an_object_that_is_not_the_module_object_is_not_found() {
    for main in [
        "const Module = require('node:module');\nconst unknown = globalThis[process.argv[5] ?? 'structuredClone'];\nconst holder = unknown({ kept: 1 });\nif (process.argv.length > 6) unknown(Module, Module.prototype);\nholder._resolveFilename = FUNCTION;\nholder.require = FUNCTION;",
        "const holder = { nodeModule: require('node:module') };\nholder._resolveFilename = FUNCTION;\nconsole.log(holder);",
        "const Module = require('node:module');\nconsole.log(Module);\nconst own = {};\nown._resolveFilename = FUNCTION;\nown.require = FUNCTION;",
        "const Module = require('node:module');\nModule._load = FUNCTION;\nModule[process.argv[2]] = FUNCTION;",
        "const Module = require('node:module');\nlet hook = Module._resolveFilename;\nhook = FUNCTION;",
    ] {
        let hooks = find_hooks_replaced_in(&main.replace("FUNCTION", FUNCTION));
        assert!(hooks.is_empty(), "{main}: {hooks:?}");
    }
}
