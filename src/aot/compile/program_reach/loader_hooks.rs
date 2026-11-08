#[cfg(test)]
mod tests;

use core::cmp::Reverse;
use core::ops::Range;

use bun_sema::atom::{Atom, known};
use bun_sema::bind::{Binding, BindingInitializer, Bound, Parent, SymbolId};
use bun_sema::check::aot::{FunctionRef, NamedValue, NamedValues};
use bun_sema::fold::{Constant, Decisions, FoldedValue};
use bun_sema::hir::{
    self, BinOp, CallId, ExprId, ExprKind, Flags, FnId, FnKind, MemberKey, NodeData, PropKey,
};
use bun_sema::import_sites::find_parameter_named;
use bun_sema::program::FileId;
use bun_sema::util::SharedSort;
use rustc_hash::FxHashMap;
use smallvec::{SmallVec, smallvec};

use super::functions::find_place_of_function;
use super::{
    FileScan, Interest, LoaderHook, ProgramReachInput, ReplacedLoaderHook, is_folded_away,
    is_loader_function,
};
use crate::builtins::table::{self, BuiltinId, MemberLookup};
use crate::known_helpers::{self, Helper, resolve_callee};
use crate::load::sources::{ParsedSource, SourceKind, parsed_source};
use crate::modules::LinkedProject;

/// A variable that is assigned more often is taken to hold anything.
const MOST_STORES_FOLLOWED: usize = 4;

type Seen = SmallVec<[SymbolId; 4]>;
type Builtins = SmallVec<[BuiltinId; 2]>;
/// The expression that a message points at, and the built-in that it may store to.
type Stores = SmallVec<[(ExprId, BuiltinId); 1]>;

/// The parameter of a function that a call hands a built-in to.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct Handed {
    function: FnId,
    place: usize,
    holder: BuiltinId,
}

fn holds_hook(holder: BuiltinId) -> bool {
    let mut members = table::members(table::canonical_id(holder)).iter();
    members.any(|&member| LoaderHook::find(member).is_some())
}

fn find_place_of_argument(hir: &hir::File<'_>, call: CallId, argument: ExprId) -> Option<usize> {
    for (place, operand) in hir.ids(hir[call].args).enumerate() {
        if matches!(hir[operand].kind, ExprKind::Spread(_)) {
            return None;
        }
        if operand == argument {
            return Some(place);
        }
    }
    None
}

/// Where a function is written, without the functions inside it.
struct OwnCode {
    written_at: Range<u32>,
    inner: Vec<Range<u32>>,
}

impl OwnCode {
    fn of(hir: &hir::File<'_>, bound: &Bound<'_>, function: FnId) -> Option<OwnCode> {
        let written_at = find_place_of_function(hir, bound, function)?;
        let others = (0..hir.fns.len() as u32).map(FnId);
        let others = others.filter(|&other| other != function);
        let places = others.filter_map(|other| find_place_of_function(hir, bound, other));
        let mut inner: Vec<Range<u32>> = places
            .filter(|place| written_at.start <= place.start && place.end <= written_at.end)
            .collect();
        inner.shared_sort_unstable_by_key(|place| (place.start, Reverse(place.end)));
        inner.dedup_by(|place, outer| place.end <= outer.end);
        Some(OwnCode { written_at, inner })
    }

    fn holds(&self, position: u32) -> bool {
        if !self.written_at.contains(&position) {
            return false;
        }
        let following = self.inner.partition_point(|place| place.start <= position);
        let before = following.checked_sub(1);
        before.is_none_or(|at| self.inner[at].end <= position)
    }
}

/// Follows the expressions of one file to the built-ins that they may name.
struct Follower<'a, 's> {
    input: &'a ProgramReachInput<'a, 's>,
    file: FileId,
    hir: &'a hir::File<'s>,
    bound: &'a Bound<'s>,
    decisions: &'a Decisions<'s>,
    source: &'a ParsedSource,
    names_of_hooks: [Option<Atom>; 2],
    handed: Option<Handed>,
}

impl<'a, 's> Follower<'a, 's> {
    fn of(
        input: &'a ProgramReachInput<'a, 's>,
        file: FileId,
        source: &'a ParsedSource,
        handed: Option<Handed>,
    ) -> Follower<'a, 's> {
        let name_of = |hook: BuiltinId| input.builtin_names.atom_of(table::entry(hook).name);
        Follower {
            input,
            file,
            hir: input.files.hir(file),
            bound: input.files.bound(file),
            decisions: &input.files.analysis(file).decisions,
            source,
            names_of_hooks: [
                name_of(BuiltinId::NodeModule_resolveFilename),
                name_of(BuiltinId::NodeModulePrototypeRequire),
            ],
            handed,
        }
    }

    fn skip_variables(&self, expression: ExprId, seen: &mut Seen) -> ExprId {
        let (hir, bound) = (self.hir, self.bound);
        let mut current = hir.skip_type_assertions(expression);
        loop {
            if !matches!(hir[current].kind, ExprKind::Ident(_))
                || bound
                    .dynamic_lookup_scope(NodeData::Expr(current))
                    .is_some()
            {
                return current;
            }
            let Binding::Symbol(symbol) = bound.binding(current) else {
                return current;
            };
            let BindingInitializer::Expression(initializer) =
                bound.binding_initializer(hir, symbol)
            else {
                return current;
            };
            if seen.contains(&symbol) {
                return current;
            }
            seen.push(symbol);
            current = hir.skip_type_assertions(initializer);
        }
    }

    fn find_value_shown_as_default(&self, object: ExprId, seen: &mut Seen) -> Option<ExprId> {
        let hir = self.hir;
        let ExprKind::Call(call) = hir[self.skip_variables(object, seen)].kind else {
            return None;
        };
        let helpers = self.input.known_helpers;
        let input = known_helpers::ResolveInput {
            files: self.input.files,
            modules: self.input.graph,
            helpers,
        };
        let callee = resolve_callee(&input, self.file, hir[call].callee)?;
        match callee.helper(helpers)? {
            Helper::ToEsModule | Helper::ImportStar | Helper::ImportDefault => {
                hir.ids(hir[call].args).next()
            }
            _ => None,
        }
    }

    fn find_value_assigned(&self, target: ExprId) -> Option<ExprId> {
        let (hir, bound) = (self.hir, self.bound);
        let target = bound.outermost_type_assertion(hir, target);
        let Parent::Expr(assignment) = bound.parent_of_expr(target) else {
            return None;
        };
        match hir[assignment].kind {
            ExprKind::Assign {
                op: None | Some(BinOp::Or | BinOp::Nullish | BinOp::And),
                target: assigned,
                value,
            } if assigned == target && !self.decisions.is_dead_expr(assignment) => Some(value),
            _ => None,
        }
    }

    fn push_values_of_variable(
        &self,
        variable: ExprId,
        seen: &mut Seen,
        found: &mut Builtins,
        values: &mut SmallVec<[ExprId; 4]>,
    ) {
        let (hir, bound) = (self.hir, self.bound);
        if bound
            .dynamic_lookup_scope(NodeData::Expr(variable))
            .is_some()
        {
            return;
        }
        let Binding::Symbol(symbol) = bound.binding(variable) else {
            return;
        };
        if let Some(handed) = self.handed
            && find_parameter_named(hir, bound, variable) == Some((handed.function, handed.place))
        {
            return found.push(handed.holder);
        }
        if seen.contains(&symbol) {
            return;
        }
        seen.push(symbol);
        if let BindingInitializer::Expression(initializer) = bound.binding_initializer(hir, symbol)
        {
            values.push(initializer);
        }
        if bound.is_stable_binding(symbol) {
            return;
        }
        let stores = bound.stores_to(symbol);
        if stores.len() <= MOST_STORES_FOLLOWED {
            let assigned = stores.iter();
            values.extend(assigned.filter_map(|store| self.find_value_assigned(store.target)));
        }
    }

    fn push_members_behind(
        &self,
        access: ExprId,
        seen: &mut Seen,
        found: &mut Builtins,
        values: &mut SmallVec<[ExprId; 4]>,
    ) {
        let Some((object, MemberKey::Name(name))) = self.hir.member_key(access) else {
            return;
        };
        if name == known::default
            && let Some(value) = self.find_value_shown_as_default(object, seen)
        {
            return values.push(value);
        }
        let Some(name) = self.input.builtin_names.get(name) else {
            return;
        };
        let mut holders = Builtins::new();
        self.push_builtins_behind(object, seen, &mut holders);
        for holder in holders {
            match table::member(holder, name) {
                MemberLookup::Found(member) => found.push(member),
                MemberLookup::Unknown | MemberLookup::Absent => {}
            }
        }
    }

    /// Each built-in that `expression` may name: through variables, choices of values and members.
    fn push_builtins_behind(&self, expression: ExprId, seen: &mut Seen, found: &mut Builtins) {
        let hir = self.hir;
        let mut values: SmallVec<[ExprId; 4]> = smallvec![expression];
        while let Some(value) = values.pop() {
            let written = hir.skip_type_assertions(value);
            if let Some(builtin) = self.source.builtins.builtin_of_expression(written) {
                found.push(builtin);
                continue;
            }
            match hir[written].kind {
                ExprKind::Ident(_) => {
                    self.push_values_of_variable(written, seen, found, &mut values);
                }
                ExprKind::Cond { branches, .. } => {
                    let branches = hir.branches(branches);
                    values.extend([branches.no, branches.yes]);
                }
                ExprKind::Binary {
                    op: BinOp::Or | BinOp::Nullish,
                    left,
                    right,
                } => values.extend([right, left]),
                ExprKind::Binary {
                    op: BinOp::And | BinOp::Comma,
                    right,
                    ..
                } => values.push(right),
                ExprKind::Dot { .. } | ExprKind::Index { .. } => {
                    self.push_members_behind(written, seen, found, &mut values);
                }
                _ => {}
            }
        }
    }

    fn names_hook(&self, access: ExprId) -> bool {
        matches!(self.hir.member_key(access), Some((_, MemberKey::Name(name)))
            if self.names_of_hooks.contains(&Some(name)))
    }

    fn push_stores_of_assignment(&self, assignment: ExprId, target: ExprId, stores: &mut Stores) {
        let hir = self.hir;
        let written = hir.skip_type_assertions(target);
        let mut parts: SmallVec<[ExprId; 4]> = smallvec![written];
        while let Some(part) = parts.pop() {
            let part = hir.skip_type_assertions(part);
            match hir[part].kind {
                ExprKind::Dot { .. } | ExprKind::Index { .. } => {
                    if self.handed.is_some() && !self.names_hook(part) {
                        continue;
                    }
                    let place = if part == written { assignment } else { part };
                    let mut stored = Builtins::new();
                    self.push_builtins_behind(part, &mut Seen::new(), &mut stored);
                    let functions = stored.into_iter().filter(|&it| is_loader_function(it));
                    stores.extend(functions.map(|function| (place, function)));
                }
                ExprKind::Array(elements) => parts.extend(hir.ids(elements)),
                ExprKind::Object(properties) => {
                    parts.extend(properties.iter().map(|property| hir[property].value));
                }
                ExprKind::Spread(rest) => parts.push(rest),
                _ => {}
            }
        }
    }

    fn find_name_of_hook(&self, key: ExprId) -> Option<Atom> {
        let key = self.hir.skip_type_assertions(key);
        let text = match self.decisions.constant(self.hir, key) {
            FoldedValue::Constant(Constant::String(text))
            | FoldedValue::ConstantOutsideDeadZone(Constant::String(text)) => text,
            FoldedValue::Constant(_)
            | FoldedValue::ConstantOutsideDeadZone(_)
            | FoldedValue::Unknown => return None,
        };
        self.names_of_hooks.contains(&Some(text)).then_some(text)
    }

    fn push_names_of_hooks_copied(&self, copied: ExprId, names: &mut SmallVec<[Atom; 2]>) {
        let hir = self.hir;
        let ExprKind::Object(properties) = hir[hir.skip_type_assertions(copied)].kind else {
            return;
        };
        for property in properties.iter() {
            match hir[property].key {
                PropKey::Name(name) if self.names_of_hooks.contains(&Some(name)) => {
                    names.push(name);
                }
                PropKey::Computed(key) => names.extend(self.find_name_of_hook(key)),
                PropKey::Name(_) | PropKey::Private(_) | PropKey::None => {}
            }
        }
    }

    /// `Object.assign(holder, { name: value })` and `Reflect.set(holder, "name", value)` store as an assignment does.
    fn push_stores_of_call(&self, expression: ExprId, call: CallId, stores: &mut Stores) {
        let hir = self.hir;
        let callee = hir.skip_type_assertions(hir[call].callee);
        let function = self.source.builtins.builtin_of_expression(callee);
        let function = function.map(table::canonical_id);
        let Some(function @ (BuiltinId::ObjectAssign | BuiltinId::ReflectSet)) = function else {
            return;
        };
        let operands: SmallVec<[ExprId; 4]> = hir.ids(hir[call].args).collect();
        let mut spreads = operands.iter();
        if spreads.any(|&operand| matches!(hir[operand].kind, ExprKind::Spread(_))) {
            return;
        }
        let mut names: SmallVec<[Atom; 2]> = SmallVec::new();
        let holder = match (function, &operands[..]) {
            (BuiltinId::ObjectAssign, &[holder, ref copied @ ..]) => {
                for &copied in copied {
                    self.push_names_of_hooks_copied(copied, &mut names);
                }
                holder
            }
            (BuiltinId::ReflectSet, &[holder, key, _])
            | (BuiltinId::ReflectSet, &[_, key, _, holder, ..]) => {
                names.extend(self.find_name_of_hook(key));
                holder
            }
            _ => return,
        };
        if names.is_empty() {
            return;
        }
        let mut holders = Builtins::new();
        self.push_builtins_behind(holder, &mut Seen::new(), &mut holders);
        for name in names
            .into_iter()
            .filter_map(|name| self.input.builtin_names.get(name))
        {
            for &holder in &holders {
                match table::member(holder, name) {
                    MemberLookup::Found(member) if LoaderHook::find(member).is_some() => {
                        stores.push((expression, member));
                    }
                    MemberLookup::Found(_) | MemberLookup::Unknown | MemberLookup::Absent => {}
                }
            }
        }
    }

    fn find_function_handed(
        &self,
        callee: ExprId,
        place: usize,
        holder: BuiltinId,
    ) -> Option<(FileId, Handed)> {
        let input = self.input;
        let project = LinkedProject {
            files: input.files,
            graph: input.graph,
            builtin_names: input.builtin_names,
            platform: input.platform,
            library: None,
        };
        let Some(NamedValue::Function(FunctionRef { file, function })) =
            project.find_named_value(self.file, callee)
        else {
            return None;
        };
        let written = &input.files.hir(file)[function];
        let runs_where_called = matches!(
            written.kind,
            FnKind::Decl | FnKind::Expr | FnKind::Arrow | FnKind::Method
        ) && !written.flags.contains(Flags::GENERATOR);
        let handed = Handed {
            function,
            place,
            holder,
        };
        (runs_where_called && input.sources.kind(file) == SourceKind::Program)
            .then_some((file, handed))
    }

    fn find_function_handed_by_call_around(&self, handed: ExprId) -> Option<(FileId, Handed)> {
        let (hir, bound) = (self.hir, self.bound);
        let holder = self.source.builtins.builtin_of_expression(handed)?;
        let argument = bound.outermost_type_assertion(hir, handed);
        let Parent::Expr(user) = bound.parent_of_expr(argument) else {
            return None;
        };
        let ExprKind::Call(call) = hir[user].kind else {
            return None;
        };
        let place = find_place_of_argument(hir, call, argument)?;
        holds_hook(holder)
            .then(|| self.find_function_handed(hir[call].callee, place, holder))
            .flatten()
    }

    fn push_functions_handed_by(&self, call: CallId, handed_on: &mut Vec<(FileId, Handed)>) {
        let hir = self.hir;
        for (place, operand) in hir.ids(hir[call].args).enumerate() {
            let operand = hir.skip_type_assertions(operand);
            match hir[operand].kind {
                ExprKind::Spread(_) => return,
                ExprKind::Ident(_) | ExprKind::Dot { .. } | ExprKind::Index { .. } => {}
                _ => continue,
            }
            let mut holders = Builtins::new();
            self.push_builtins_behind(operand, &mut Seen::new(), &mut holders);
            for holder in holders.into_iter().filter(|&holder| holds_hook(holder)) {
                handed_on.extend(self.find_function_handed(hir[call].callee, place, holder));
            }
        }
    }
}

/// What each function that was handed a holder of hooks stores, with the functions that it hands the holder on to.
#[derive(Default)]
struct Followed {
    hooks_replaced: FxHashMap<(FileId, Handed), Vec<ReplacedLoaderHook>>,
}

impl Followed {
    fn find_hooks_replaced_behind(
        &mut self,
        follower: &Follower<'_, '_>,
        handed: ExprId,
    ) -> &[ReplacedLoaderHook] {
        let Some(first) = follower.find_function_handed_by_call_around(handed) else {
            return &[];
        };
        let input = follower.input;
        self.hooks_replaced.entry(first).or_insert_with(|| {
            let (mut entered, mut replaced) = (vec![first], Vec::new());
            let mut next = 0;
            while let Some(&(file, handed)) = entered.get(next) {
                next += 1;
                let mut handed_on = Vec::new();
                find_hooks_replaced_in(input, file, handed, &mut replaced, &mut handed_on);
                for function in handed_on {
                    if !entered.contains(&function) {
                        entered.push(function);
                    }
                }
            }
            replaced
        })
    }
}

fn find_hooks_replaced_in(
    input: &ProgramReachInput<'_, '_>,
    file: FileId,
    handed: Handed,
    replaced: &mut Vec<ReplacedLoaderHook>,
    handed_on: &mut Vec<(FileId, Handed)>,
) {
    let Some(source) = parsed_source(input.files, file) else {
        return;
    };
    let follower = Follower::of(input, file, source, Some(handed));
    let hir = follower.hir;
    let Some(own_code) = OwnCode::of(hir, follower.bound, handed.function) else {
        return;
    };
    let is_run = |expression: ExprId| {
        own_code.holds(hir[expression].pos) && !follower.decisions.is_dead_expr(expression)
    };
    let mut stores = Stores::new();
    for expression in (0..hir.exprs.len() as u32).map(ExprId) {
        match hir[expression].kind {
            ExprKind::Assign { target, .. } if is_run(expression) => {
                follower.push_stores_of_assignment(expression, target, &mut stores);
            }
            ExprKind::Call(call) if is_run(expression) => {
                follower.push_stores_of_call(expression, call, &mut stores);
                follower.push_functions_handed_by(call, handed_on);
            }
            _ => {}
        }
    }
    for (store, stored) in stores {
        if let Some(hook) = LoaderHook::find(stored)
            && !is_folded_away(follower.decisions, follower.bound, store)
        {
            replaced.push(ReplacedLoaderHook { file, store, hook });
        }
    }
}

impl FileScan<'_, '_> {
    pub(super) fn add_stores_of_loader_functions(&mut self) {
        let (hir, file) = (self.hir, self.file);
        let Some(source) = parsed_source(self.input.files, file) else {
            return;
        };
        let builtins = &source.builtins;
        let mut mentioned = builtins.mentioned().iter();
        let names_module =
            mentioned.any(|&builtin| table::canonical_id(builtin) == BuiltinId::NodeModule);
        if !names_module && builtins.tables_of_loaders_written().is_empty() {
            return;
        }
        let follower = Follower::of(self.input, file, source, None);
        let decisions = follower.decisions;
        let mut followed = Followed::default();
        for &table in builtins.tables_of_loaders_written() {
            if decisions.is_dead_expr(table) || is_folded_away(decisions, follower.bound, table) {
                continue;
            }
            self.add_interest(hir[table].pos, Interest::StoreOfLoaderFunction(None));
            for &replaced in followed.find_hooks_replaced_behind(&follower, table) {
                let interest = Interest::StoreOfLoaderFunction(Some(replaced));
                self.add_interest(hir[table].pos, interest);
            }
        }
        if !names_module {
            return;
        }
        let mut mentioned = builtins.mentioned().iter();
        let names_call_that_stores = mentioned.any(|&builtin| {
            let function = table::canonical_id(builtin);
            matches!(function, BuiltinId::ObjectAssign | BuiltinId::ReflectSet)
        });
        let mut stores = Stores::new();
        for expression in self.expressions() {
            match hir[expression].kind {
                ExprKind::Assign { target, .. } if !decisions.is_dead_expr(expression) => {
                    follower.push_stores_of_assignment(expression, target, &mut stores);
                }
                ExprKind::Call(call)
                    if names_call_that_stores && !decisions.is_dead_expr(expression) =>
                {
                    follower.push_stores_of_call(expression, call, &mut stores);
                }
                _ => continue,
            }
            for (store, stored) in stores.drain(..) {
                if is_folded_away(decisions, follower.bound, store) {
                    continue;
                }
                let hook = LoaderHook::find(stored);
                let replaced = hook.map(|hook| ReplacedLoaderHook { file, store, hook });
                self.add_interest(hir[store].pos, Interest::StoreOfLoaderFunction(replaced));
            }
        }
    }
}
