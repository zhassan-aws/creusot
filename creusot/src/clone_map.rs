#![allow(deprecated)]

use heck::ToUpperCamelCase;
use indexmap::{IndexMap, IndexSet};
use petgraph::{graphmap::DiGraphMap, visit::DfsPostOrder, EdgeDirection::Outgoing};
use rustc_hir::{def::DefKind, def_id::DefId};
use rustc_middle::ty::{
    self,
    subst::{GenericArgKind, InternalSubsts, SubstsRef},
    AliasKind, AliasTy, DefIdTree, EarlyBinder, ParamEnv, Ty, TyCtxt, TyKind, TypeFoldable,
    TypeSuperVisitable, TypeVisitor,
};
use rustc_resolve::Namespace;
use rustc_span::{Symbol, DUMMY_SP};
use why3::{
    declaration::{CloneKind, CloneSubst, Decl, DeclClone, Use},
    Ident, QName,
};

use crate::{
    backend::dependency::Dependency,
    ctx::{self, *},
    translation::interface,
    util::{self, get_builtin, ident_of, ident_of_ty, item_name, module_name},
};

// Prelude modules
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PreludeModule {
    Float32,
    Float64,
    Int,
    Int8,
    Int16,
    Int32,
    Int64,
    Int128,
    Isize,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    UInt128,
    Usize,
    Char,
    Bool,
    Borrow,
    Slice,
    Opaque,
    Ref,
    Seq,
    Type,
}

impl PreludeModule {
    fn qname(&self) -> QName {
        match self {
            PreludeModule::Float32 => QName::from_string("prelude.Float32").unwrap(),
            PreludeModule::Float64 => QName::from_string("prelude.Float64").unwrap(),
            PreludeModule::Int => QName::from_string("prelude.Int").unwrap(),
            PreludeModule::Int8 => QName::from_string("prelude.Int8").unwrap(),
            PreludeModule::Int16 => QName::from_string("prelude.Int16").unwrap(),
            PreludeModule::Int32 => QName::from_string("prelude.Int32").unwrap(),
            PreludeModule::Int64 => QName::from_string("prelude.Int64").unwrap(),
            PreludeModule::Int128 => QName::from_string("prelude.Int128").unwrap(),
            PreludeModule::UInt8 => QName::from_string("prelude.UInt8").unwrap(),
            PreludeModule::UInt16 => QName::from_string("prelude.UInt16").unwrap(),
            PreludeModule::UInt32 => QName::from_string("prelude.UInt32").unwrap(),
            PreludeModule::UInt64 => QName::from_string("prelude.UInt64").unwrap(),
            PreludeModule::UInt128 => QName::from_string("prelude.UInt128").unwrap(),
            PreludeModule::Char => QName::from_string("prelude.Char").unwrap(),
            PreludeModule::Opaque => QName::from_string("prelude.Opaque").unwrap(),
            PreludeModule::Ref => QName::from_string("Ref").unwrap(),
            PreludeModule::Seq => QName::from_string("prelude.Seq").unwrap(),
            PreludeModule::Type => QName::from_string("Type").unwrap(),
            PreludeModule::Isize => QName::from_string("prelude.IntSize").unwrap(),
            PreludeModule::Usize => QName::from_string("prelude.UIntSize").unwrap(),
            PreludeModule::Bool => QName::from_string("prelude.Bool").unwrap(),
            PreludeModule::Borrow => QName::from_string("prelude.Borrow").unwrap(),
            PreludeModule::Slice => QName::from_string("prelude.Slice").unwrap(),
        }
    }
}

type CloneNode<'tcx> = (DefId, SubstsRef<'tcx>);
pub type CloneSummary<'tcx> = IndexMap<(DefId, SubstsRef<'tcx>), CloneInfo>;

#[derive(Clone)]
pub struct CloneMap<'tcx> {
    tcx: TyCtxt<'tcx>,
    prelude: IndexMap<QName, bool>,
    pub names: IndexMap<CloneNode<'tcx>, CloneInfo>,

    // Track how many instances of a name already exist
    name_counts: IndexMap<Symbol, usize>,

    // Indicates the desired level of information in clones
    // - Stub: serves purely in logical function definitions to get around the limitations of `clone`
    // - Interface: Will clone only the interface of used modules
    // - Body: Will directly use the full body of dependencies, except for program functions
    clone_level: CloneLevel,

    // DefId of the item which is cloning. Used for trait resolution
    self_id: DefId,
    // TODO: Push the graph into an opaque type with tight api boundary
    // Graph which is used to calculate the full clone set
    clone_graph: DiGraphMap<DepNode<'tcx>, IndexSet<(Kind, SymbolKind)>>,
    // Index of the last cloned entry
    last_cloned: usize,

    // Internal state to determine whether clones should be public or not
    public: bool,

    // Used to ensure we only have a single `use` per type.
    used_types: IndexSet<DefId>,
}

impl<'tcx> Drop for CloneMap<'tcx> {
    fn drop(&mut self) {
        if self.last_cloned != self.names.len() {
            debug!(
                "Dropping clone map with un-emitted clones. {:?} clones emitted of {:?} total {:?}",
                self.last_cloned,
                self.names.len(),
                self.self_id,
            );
        }
    }
}

type DepNode<'tcx> = Dependency<'tcx>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, TyEncodable, TyDecodable, Hash)]
enum Kind {
    Named(Symbol),
    Hidden,
    Export,
}

impl Kind {
    pub(crate) fn qname_ident(&self, method: Ident) -> QName {
        let module = match &self {
            Kind::Named(name) => vec![name.to_string().into()],
            _ => Vec::new(),
        };
        QName { module, name: method }
    }
}

use rustc_macros::{TyDecodable, TyEncodable};

#[derive(Clone, Copy, Debug, PartialEq, Eq, TyEncodable, TyDecodable)]
enum CloneOpacity {
    Transparent,
    Opaque,
    Default,
}

#[derive(Clone, Debug, TyEncodable, TyDecodable)]
pub struct CloneInfo {
    kind: Kind,
    cloned: bool,
    public: bool,
    opaque: CloneOpacity,
}

impl Into<CloneKind> for Kind {
    fn into(self) -> CloneKind {
        match self {
            Kind::Named(i) => CloneKind::Named(i.to_string().into()),
            Kind::Hidden => CloneKind::Bare,
            Kind::Export => CloneKind::Export,
        }
    }
}

impl<'tcx> CloneInfo {
    fn from_name(name: Symbol, public: bool) -> Self {
        CloneInfo { kind: Kind::Named(name), cloned: false, public, opaque: CloneOpacity::Default }
    }

    fn hidden() -> Self {
        CloneInfo {
            kind: Kind::Hidden,
            cloned: false,
            public: false,
            opaque: CloneOpacity::Default,
        }
    }

    pub(crate) fn opaque(&mut self) {
        self.opaque = CloneOpacity::Opaque;
    }

    fn qname_ident(&self, method: Ident) -> QName {
        self.kind.qname_ident(method)
    }
}

impl<'tcx> CloneMap<'tcx> {
    pub(crate) fn new(tcx: TyCtxt<'tcx>, self_id: DefId, clone_level: CloneLevel) -> Self {
        let names = IndexMap::new();
        let mut c = CloneMap {
            tcx,
            self_id,
            names,
            name_counts: Default::default(),
            prelude: IndexMap::new(),
            clone_level,
            clone_graph: DiGraphMap::new(),
            last_cloned: 0,
            public: false,
            used_types: Default::default(),
        };
        debug!("cloning self: {:?}", c.self_key());
        c.names.insert(c.self_key(), CloneInfo::hidden());
        c
    }

    pub(crate) fn summary(&self) -> CloneSummary<'tcx> {
        self.names
            .iter()
            .filter_map(|(k, ci)| match &ci.kind {
                Kind::Named(_) => Some((*k, ci.clone())),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn with_public_clones<F, A>(&mut self, f: F) -> A
    where
        F: FnOnce(&mut Self) -> A,
    {
        let public = std::mem::replace(&mut self.public, true);
        let ret = f(self);
        self.public = public;
        ret
    }

    /// Internal: only meant for mutually recursive type declaration
    pub(crate) fn insert_hidden(&mut self, def_id: DefId, subst: SubstsRef<'tcx>) {
        let subst = self.tcx.erase_regions(subst);
        self.names.insert((def_id, subst), CloneInfo::hidden());
    }

    #[deprecated(
        note = "Avoid using this method in favor of one of the more semantic alternatives: `value`, `accessor`, `ty`"
    )]
    pub(crate) fn insert(&mut self, def_id: DefId, subst: SubstsRef<'tcx>) -> &mut CloneInfo {
        let subst = self.tcx.erase_regions(subst);

        let (def_id, subst) = self.closure_hack(def_id, subst);

        self.names.entry((def_id, subst)).or_insert_with(|| {
            let base_sym = match util::item_type(self.tcx, def_id) {
                ItemType::Impl => self.tcx.item_name(self.tcx.trait_id_of_impl(def_id).unwrap()),
                ItemType::Closure => Symbol::intern(&format!(
                    "closure{}",
                    self.tcx.def_path(def_id).data.last().unwrap().disambiguator
                )),
                _ => self.tcx.item_name(def_id),
            };

            let base = Symbol::intern(&base_sym.as_str().to_upper_camel_case());
            let count: usize = *self.name_counts.entry(base).and_modify(|c| *c += 1).or_insert(0);
            trace!("inserting {:?} {:?} as {}{}", def_id, subst, base, count);

            let name = if util::item_type(self.tcx, def_id) == ItemType::Type {
                Symbol::intern(&*module_name(self.tcx, def_id))
            } else {
                Symbol::intern(&format!("{}{}", base, count))
            };

            let info = CloneInfo::from_name(name, self.public);
            info
        })
    }

    pub(crate) fn value(&mut self, def_id: DefId, subst: SubstsRef<'tcx>) -> QName {
        let name = item_name(self.tcx, def_id, Namespace::ValueNS);
        self.insert(def_id, subst).qname_ident(name.into())
    }

    pub(crate) fn ty(&mut self, def_id: DefId, subst: SubstsRef<'tcx>) -> QName {
        let name = item_name(self.tcx, def_id, Namespace::TypeNS);
        self.insert(def_id, subst).qname_ident(name.into())
    }

    pub(crate) fn constructor(&mut self, def_id: DefId, subst: SubstsRef<'tcx>) -> QName {
        let type_id = match self.tcx.def_kind(def_id) {
            DefKind::Closure | DefKind::Struct | DefKind::Enum | DefKind::Union => def_id,
            DefKind::Variant => self.tcx.parent(def_id),
            _ => unreachable!("Not a type or constructor"),
        };
        let mut name = item_name(self.tcx, def_id, Namespace::ValueNS);
        name.capitalize();
        self.insert(type_id, subst).qname_ident(name.into())
    }

    /// Creates a name for a type or closure projection ie: x.field1
    /// This also includes projections from `enum` types
    ///
    /// * `def_id` - The id of the type or closure being projected
    /// * `subst` - Substitution that type is being accessed at
    /// * `variant` - The constructor being used. For closures this is always 0
    /// * `ix` - The field in that constructor being accessed.
    pub(crate) fn accessor(
        &mut self,
        def_id: DefId,
        subst: SubstsRef<'tcx>,
        variant: usize,
        ix: usize,
    ) -> QName {
        let tcx = self.tcx;
        let clone = self.insert(def_id, subst);
        let name: Ident = match util::item_type(tcx, def_id) {
            ItemType::Closure => format!("field_{}", ix).into(),
            ItemType::Type => {
                let variant_def = &tcx.adt_def(def_id).variants()[variant.into()];
                let variant = variant_def;
                format!(
                    "{}_{}",
                    variant.name.as_str().to_ascii_lowercase(),
                    variant.fields[ix].name
                )
                .into()
            }
            _ => panic!("accessor: invalid item kind"),
        };

        clone.qname_ident(name.into())
    }

    fn self_key(&self) -> (DefId, SubstsRef<'tcx>) {
        let subst = match self.tcx.def_kind(self.self_id) {
            DefKind::Closure => match self.tcx.type_of(self.self_id).kind() {
                TyKind::Closure(_, subst) => subst,
                _ => unreachable!(),
            },
            _ => InternalSubsts::identity_for_item(self.tcx, self.self_id),
        };

        let subst = self.tcx.erase_regions(subst);
        (self.self_id, subst)
    }

    pub(crate) fn import_prelude_module(&mut self, module: PreludeModule) {
        self.prelude.entry(module.qname()).or_insert(false);
    }

    pub(crate) fn import_builtin_module(&mut self, module: QName) {
        self.prelude.entry(module).or_insert(false);
    }

    fn closure_hack(&self, def_id: DefId, subst: SubstsRef<'tcx>) -> (DefId, SubstsRef<'tcx>) {
        if self.tcx.is_diagnostic_item(Symbol::intern("fn_once_impl_precond"), def_id)
            || self.tcx.is_diagnostic_item(Symbol::intern("fn_once_impl_postcond"), def_id)
            || self.tcx.is_diagnostic_item(Symbol::intern("fn_mut_impl_postcond"), def_id)
            || self.tcx.is_diagnostic_item(Symbol::intern("fn_impl_postcond"), def_id)
            || self.tcx.is_diagnostic_item(Symbol::intern("fn_mut_impl_unnest"), def_id)
            || self.tcx.is_diagnostic_item(Symbol::intern("fn_impl_resolve"), def_id)
        {
            trace!("closure_hack: {:?} {:?}", self.self_id, def_id);
            let self_ty = subst.types().nth(1).unwrap();
            if let TyKind::Closure(id, csubst) = self_ty.kind() {
                return (*id, csubst);
            }
        };

        if self.tcx.is_diagnostic_item(Symbol::intern("creusot_resolve_default"), def_id)
            || self.tcx.is_diagnostic_item(Symbol::intern("creusot_resolve_method"), def_id)
        {
            let self_ty = subst.types().nth(0).unwrap();
            if let TyKind::Closure(id, csubst) = self_ty.kind() {
                return (*id, csubst);
            }
        }

        (def_id, subst)
    }

    // Update the clone graph with new entries
    fn update_graph(&mut self, ctx: &mut ctx::TranslationCtx<'tcx>) {
        // Construct a maximal sharing graph for all dependencies.
        // We build edges between each (function, subst) pair, following the call graph
        // Additionally, when the substitution refers to an associated type, we construct
        // a relevant edge.
        //
        // Along the edge, we include the 'original' substitution, which we can use
        // to build the correct substitution.
        //
        let mut i = self.last_cloned;
        while i < self.names.len() {
            let key = *self.names.get_index(i).unwrap().0;

            i += 1;
            trace!("{:?} is public={:?}", key, self.names[&key].public);

            if key != self.self_key() {
                self.add_graph_edge(DepNode::Item(self.self_key()), DepNode::Item(key));
            }

            if self.names[&key].kind == Kind::Hidden {
                continue;
            }

            if still_specializable(self.tcx, key.0, key.1) {
                self.names[&key].opaque();
            }

            ctx.translate(key.0);

            trace!(
                "{:?} {:?} has {:?} dependencies",
                self.names[&key].kind,
                key,
                ctx.dependencies(key.0).map(|d| d.len())
            );
            self.clone_laws(ctx, key);
            self.clone_dependencies(ctx, key);
        }
    }

    fn clone_dependencies(
        &mut self,
        ctx: &mut TranslationCtx<'tcx>,
        key: (DefId, SubstsRef<'tcx>),
    ) {
        // Check the substitution for dependencies on closures
        for ty in key.1.types().flat_map(|t| t.walk()) {
            let ty = match ty.unpack() {
                GenericArgKind::Type(ty) => ty,
                _ => continue,
            };
            match ty.kind() {
                TyKind::Closure(id, subst) => {
                    self.insert(*id, subst);
                    // Sketchy... shouldn't we need to do something to subst?
                    self.add_graph_edge(DepNode::Item(key), DepNode::Item((*id, subst)));
                }
                TyKind::Adt(def, subst) => {
                    self.insert(def.did(), subst);
                    self.add_graph_edge(DepNode::Item(key), DepNode::Item((def.did(), subst)));
                }
                _ => {}
            }
        }
        walk_projections(key.1, |pty: &AliasTy<'tcx>| {
            let dep = self.resolve_dep(ctx, (pty.def_id, pty.substs));

            if let Some((defid, subst)) = dep.cloneable_id() {
                self.insert(defid, subst);
            }

            self.add_graph_edge(DepNode::Item(key), dep);
        });

        let key_public = self.names[&key].public;

        let opaque_clone = !matches!(self.clone_level, CloneLevel::Body)
            || self.names[&key].opaque == CloneOpacity::Opaque;

        for (dep, info) in ctx.dependencies(key.0).iter().flat_map(|i| i.iter()) {
            if opaque_clone && !info.public {
                continue;
            }
            trace!("adding dependency {:?} {:?}", dep, info.public);

            let orig = dep;

            let dep = self.resolve_dep(ctx, (dep.0, EarlyBinder(dep.1).subst(self.tcx, key.1)));

            if let Some((defid, subst)) = dep.cloneable_id() {
                trace!("inserting dependency {:?} {:?}", key, dep);

                self.insert(defid, subst).public |= key_public && info.public;
            }

            // Skip reflexive edges
            if dep == DepNode::Item(key) {
                continue;
            }

            let edge_set = self.add_graph_edge(DepNode::Item(key), dep);
            if let Some(sym) = refineable_symbol(ctx.tcx, orig.0) {
                edge_set.insert((info.kind, sym));
            }
        }
    }

    // Adds a dependency from `user` on `prov` for the symbol `sym`.
    fn add_graph_edge(
        &mut self,
        user: DepNode<'tcx>,
        prov: DepNode<'tcx>,
    ) -> &mut IndexSet<(Kind, SymbolKind)> {
        let k1 =
            if let Some(d1) = user.cloneable_id() { Some(&self.names[&d1].kind) } else { None };
        let k2 =
            if let Some(d2) = prov.cloneable_id() { Some(&self.names[&d2].kind) } else { None };
        trace!("{k1:?} = {:?} --> {k2:?} = {:?}", user, prov);

        if let None = self.clone_graph.edge_weight_mut(user, prov) {
            self.clone_graph.add_edge(user, prov, IndexSet::new());
        };

        self.clone_graph.edge_weight_mut(user, prov).unwrap()
    }

    // Given an initial substitution, find out the substituted and resolved version of the dependency `dep`.
    // This will attempt to normalize traits and associated types if the substitution provides enough
    // information.
    fn resolve_dep(
        &self,
        ctx: &TranslationCtx<'tcx>,
        dep: (DefId, SubstsRef<'tcx>),
    ) -> DepNode<'tcx> {
        let param_env = ctx.param_env(self.self_id);

        let dep = match util::item_type(self.tcx, dep.0) {
            ItemType::Type => Dependency::Type(ctx.mk_adt(ctx.adt_def(dep.0), dep.1)),
            ItemType::AssocTy => Dependency::Type(ctx.mk_projection(dep.0, dep.1)),
            _ => Dependency::Item(dep),
        };
        dep.resolve(ctx, param_env).unwrap_or(dep)
    }

    fn clone_laws(&mut self, ctx: &mut TranslationCtx<'tcx>, key: (DefId, SubstsRef<'tcx>)) {
        let Some(item) = ctx.tcx.opt_associated_item(key.0) else { return };

        // Dont clone laws into the trait / impl which defines them.
        if let Some(self_trait) = ctx.tcx.opt_associated_item(self.self_id) {
            if self_trait.container_id(ctx.tcx) == item.container_id(ctx.tcx) {
                return;
            }
        }

        // If the function we are cloning into is `#[trusted]` there is no need for laws.
        // Similarily, if it has no body, there will be no proofs.
        if util::is_trusted(ctx.tcx, self.self_id) || !util::has_body(ctx, self.self_id) {
            return;
        }

        if self.clone_level == CloneLevel::Stub {
            return;
        }

        let laws = ctx.laws(item.container_id(ctx.tcx));

        for law in laws {
            trace!("adding law {:?} in {:?}", *law, self.self_id);

            // No way the substitution is correct...
            let law = self.insert(*law, key.1);
            law.public = false;
        }
    }

    fn build_clone(&mut self, ctx: &mut TranslationCtx<'tcx>, item: DepNode<'tcx>) -> Option<Decl> {
        let node @ (def_id, subst) = item.cloneable_id()?;

        // Types can't be cloned, but are used (for now).
        if util::item_type(ctx.tcx, def_id) == ItemType::Type {
            if self.used_types.insert(def_id) {
                let name = if let Some(builtin) = get_builtin(ctx.tcx, def_id) {
                    let name = QName::from_string(&builtin.as_str()).unwrap().module_qname();

                    Decl::UseDecl(Use { name: name.clone(), as_: None, export: false })
                } else {
                    let name = cloneable_name(ctx, def_id, CloneLevel::Body);
                    Decl::UseDecl(Use { name: name.clone(), as_: Some(name), export: false })
                };
                return Some(name);
            }
            return None;
        }

        let mut clone_subst = base_subst(ctx, self, ctx.param_env(self.self_id), def_id, subst);

        let outbound: Vec<_> =
            self.clone_graph.neighbors_directed(DepNode::Item(node), Outgoing).collect();

        // Grab definitions from all of our dependencies
        for dep in outbound {
            let syms = &self.clone_graph[(DepNode::Item(node), dep)];
            trace!("s={:?} t={:?} e={:?}", dep, node, syms);

            match dep {
                DepNode::Type(ty) => {
                    for (nm, sym) in syms.clone() {
                        let ty_name = nm.qname_ident(sym.ident());
                        let ty = super::ty::translate_ty(ctx, self, DUMMY_SP, ty);
                        clone_subst.push(CloneSubst::Type(ty_name, ty))
                    }
                }
                DepNode::Item(dep) => {
                    for (nm, sym) in syms {
                        let elem = sym.to_subst(*nm, self.names[&dep].kind);
                        clone_subst.push(elem);
                    }
                }
            }
        }

        let use_axioms = ctx.item(def_id).map(|i| i.has_axioms()).unwrap_or(false);
        if use_axioms {
            clone_subst.push(CloneSubst::Axiom(None))
        }

        let interface = match (self.clone_level, self.names[&node].opaque) {
            (CloneLevel::Body, CloneOpacity::Opaque) => CloneLevel::Interface,
            (x, _) => x,
        };

        trace!(
            "emit clone node={node:?} name={:?} as={:?}",
            cloneable_name(ctx, def_id, interface),
            self.names[&node].kind.clone()
        );

        Some(Decl::Clone(DeclClone {
            name: cloneable_name(ctx, def_id, interface),
            subst: clone_subst,
            kind: self.names[&node].kind.clone().into(),
        }))
    }

    pub(crate) fn to_clones(
        mut self,
        ctx: &mut ctx::TranslationCtx<'tcx>,
    ) -> (Vec<Decl>, CloneSummary<'tcx>) {
        trace!("emitting clones for {:?}", self.self_id);
        let mut decls = Vec::new();

        use petgraph::visit::Walker;

        // Update the clone graph with any new entries.
        self.update_graph(ctx);

        trace!(
            "dep_graph processed={} nodes={} edges={}",
            self.last_cloned,
            self.clone_graph.node_count(),
            self.clone_graph.edge_count()
        );

        self.last_cloned = self.names.len();

        // Broken because of closures which share a defid for the type *and* function
        // debug_assert!(!is_cyclic_directed(&self.clone_graph), "clone graph for {:?} is cyclic", self.self_id );

        let mut topo = DfsPostOrder::new(&self.clone_graph, DepNode::Item(self.self_key()));
        while let Some(node) = topo.walk_next(&self.clone_graph) {
            let Some(item) = node.cloneable_id() else { continue };
            trace!("processing node {:?}", self.names[&item].kind);

            if std::mem::replace(&mut self.names[&item].cloned, true) {
                continue;
            }

            if self.names[&item].kind == Kind::Hidden {
                continue;
            }

            let Some(decl) = self.build_clone(ctx, node) else { continue };
            decls.push(decl);
        }

        // debug_assert!(topo.finished.len() >= self.names.len(), "missed a clone in {:?}", self.self_id);

        let clones = self
            .prelude
            .iter_mut()
            .filter(|(_, v)| !(**v))
            .map(|(p, v)| {
                *v = true;
                p
            })
            .map(|q| Decl::UseDecl(Use { name: q.clone(), as_: None, export: false }))
            .chain(decls.into_iter())
            .collect();
        (clones, self.summary())
    }
}

// Create the substitution used to clone `def_id` with the rustc substitution `subst`.
pub(crate) fn base_subst<'tcx>(
    ctx: &mut TranslationCtx<'tcx>,
    names: &mut CloneMap<'tcx>,
    param_env: ParamEnv<'tcx>,
    mut def_id: DefId,
    subst: SubstsRef<'tcx>,
) -> Vec<CloneSubst> {
    use heck::ToSnakeCase;
    use rustc_middle::ty::GenericParamDefKind;
    loop {
        if ctx.tcx.is_closure(def_id) {
            def_id = ctx.tcx.parent(def_id);
        } else {
            break;
        }
    }
    let trait_params = ctx.tcx.generics_of(def_id);
    let mut clone_subst = Vec::new();

    if subst.is_empty() {
        return Vec::new();
    }

    for ix in 0..trait_params.count() {
        let p = trait_params.param_at(ix, ctx.tcx);
        let ty = subst[ix];
        if let GenericParamDefKind::Type { .. } = p.kind {
            let ty = ctx.normalize_erasing_regions(param_env, ty.expect_ty());
            let ty = super::ty::translate_ty(ctx, names, rustc_span::DUMMY_SP, ty);
            clone_subst.push(CloneSubst::Type(p.name.to_string().to_snake_case().into(), ty));
        }
    }

    clone_subst
}

// Which kind of module should we clone
// TODO: Unify with `CloneOpacity`
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum CloneLevel {
    Stub,
    Interface,
    Body,
}

fn cloneable_name(ctx: &TranslationCtx, def_id: DefId, interface: CloneLevel) -> QName {
    use util::ItemType::*;

    // TODO: Refactor.
    match util::item_type(ctx.tcx, def_id) {
        Logic | Predicate | Impl => match interface {
            CloneLevel::Stub => QName {
                module: Vec::new(),
                name: format!("{}_Stub", &*module_name(ctx.tcx, def_id)).into(),
            },
            // Why do we do this? Why not use the stub here as well?
            CloneLevel::Interface => interface::interface_name(ctx, def_id).into(),
            CloneLevel::Body => module_name(ctx.tcx, def_id).into(),
        },
        Constant => match interface {
            CloneLevel::Body => module_name(ctx.tcx, def_id).into(),
            _ => QName {
                module: Vec::new(),
                name: format!("{}_Stub", &*module_name(ctx.tcx, def_id)).into(),
            },
        },

        Program | Closure => {
            QName { module: Vec::new(), name: interface::interface_name(ctx, def_id) }
        }
        Trait | Type | AssocTy => module_name(ctx.tcx, def_id).into(),
        Unsupported(_) => unreachable!(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SymbolKind {
    Val(Symbol),
    Type(Symbol),
    Function(Symbol),
    Predicate(Symbol),
    Const(Symbol),
}

impl SymbolKind {
    fn sym(&self) -> Symbol {
        match self {
            SymbolKind::Val(i) => *i,
            SymbolKind::Type(i) => *i,
            SymbolKind::Function(i) => *i,
            SymbolKind::Predicate(i) => *i,
            SymbolKind::Const(i) => *i,
        }
    }

    fn ident(&self) -> Ident {
        match self {
            SymbolKind::Type(_) => ident_of_ty(self.sym()),
            _ => ident_of(self.sym()),
        }
    }

    fn to_subst(self, src: Kind, tgt: Kind) -> CloneSubst {
        let id = self.ident();
        match self {
            SymbolKind::Val(_) => CloneSubst::Val(src.qname_ident(id.clone()), tgt.qname_ident(id)),
            SymbolKind::Type(_) => CloneSubst::Type(
                src.qname_ident(id.clone()),
                why3::ty::Type::TConstructor(tgt.qname_ident(id)),
            ),
            SymbolKind::Function(_) => {
                CloneSubst::Function(src.qname_ident(id.clone()), tgt.qname_ident(id))
            }
            SymbolKind::Predicate(_) => {
                CloneSubst::Predicate(src.qname_ident(id.clone()), tgt.qname_ident(id))
            }
            SymbolKind::Const(_) => {
                // TMP
                CloneSubst::Val(src.qname_ident(id.clone()), tgt.qname_ident(id))
            }
        }
    }
}

// Identify the name and kind of symbol which can be refined in a given defid
fn refineable_symbol<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId) -> Option<SymbolKind> {
    use util::ItemType::*;
    match util::item_type(tcx, def_id) {
        Logic => Some(SymbolKind::Function(tcx.item_name(def_id))),
        Predicate => Some(SymbolKind::Predicate(tcx.item_name(def_id))),
        Program => Some(SymbolKind::Val(tcx.item_name(def_id))),
        AssocTy => match tcx.associated_item(def_id).container {
            ty::TraitContainer => Some(SymbolKind::Type(tcx.item_name(def_id))),
            ty::ImplContainer => None,
        },
        Trait | Impl => unreachable!("trait blocks have no refinable symbols"),
        Type => None,
        Constant => Some(SymbolKind::Const(tcx.item_name(def_id))),
        _ => unreachable!(),
    }
}

// | Final | Still Spec (Ty)| Res |
// | T | _ | F |
// | F | T | T |
// | F | F | F |

// We consider an item to be further specializable if it is provided by a parameter bound (ie: `I : Iterator`).
fn still_specializable<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId, substs: SubstsRef<'tcx>) -> bool {
    use rustc_middle::ty::TypeVisitable;
    if let Some(trait_id) = tcx.trait_of_item(def_id) {
        let is_final = tcx.impl_defaultness(def_id).is_final();
        let trait_generics = substs.truncate_to(tcx, tcx.generics_of(trait_id));
        !is_final && trait_generics.still_further_specializable()
    } else if let Some(impl_id) = tcx.impl_of_method(def_id) && tcx.trait_id_of_impl(impl_id).is_some() {
        let is_final = tcx.impl_defaultness(def_id).is_final();
        let trait_ref = tcx.impl_trait_ref(impl_id).unwrap();
        !is_final && trait_ref.subst(tcx, substs).still_further_specializable()
    } else {
        false
    }
}

// Walk all the projections in a substitution so we can add dependencies on them
fn walk_projections<'tcx, T: TypeFoldable<'tcx>, F: FnMut(&AliasTy<'tcx>)>(s: T, f: F) {
    s.visit_with(&mut ProjectionTyVisitor { f, p: std::marker::PhantomData });
}

struct ProjectionTyVisitor<'tcx, F: FnMut(&AliasTy<'tcx>)> {
    f: F,
    p: std::marker::PhantomData<&'tcx ()>,
}

impl<'tcx, F: FnMut(&AliasTy<'tcx>)> TypeVisitor<'tcx> for ProjectionTyVisitor<'tcx, F> {
    fn visit_ty(&mut self, t: Ty<'tcx>) -> std::ops::ControlFlow<Self::BreakTy> {
        match t.kind() {
            TyKind::Alias(AliasKind::Projection, pty) => {
                (self.f)(pty);
                t.super_visit_with(self)
            }
            _ => t.super_visit_with(self),
        }
    }
}
