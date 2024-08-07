use proc_macro2::Span;
use quote::ToTokens;
use std::{iter, mem::replace};
use syn::{punctuated::Punctuated, spanned::Spanned, visit_mut::*, *};

use crate::{expr::*, generics::*, helpers::*};

pub struct ParamSubst<'a, 'b> {
    pub param: &'a GenericParam,
    pub arg: ParamSubstArg<'b>,
    pub result: Result<Vec<Span>>,
}

#[derive(Clone, Copy)]
pub enum ParamSubstArg<'a> {
    TestOnly,
    Param(&'a GenericParam),
    Arg(&'a GenericArgument),
}

impl VisitMut for ParamSubst<'_, '_> {
    fn visit_lifetime_mut(&mut self, i: &mut Lifetime) {
        if let GenericParam::Lifetime(lifetime_param) = self.param {
            if i.ident == lifetime_param.lifetime.ident {
                return self.subst(i, self.arg.get_lifetime(i.span()));
            }
        }
        visit_lifetime_mut(self, i)
    }

    fn visit_expr_mut(&mut self, i: &mut Expr) {
        if let GenericParam::Const(const_param) = self.param {
            let ident = &const_param.ident;
            if let Expr::Path(expr_path) = &i {
                if expr_path.qself.is_none() && expr_path.path.is_ident(ident) {
                    return self.subst(i, self.arg.get_expr(i.span()));
                }
            } else if !matches!(self.arg, ParamSubstArg::TestOnly) {
                let span = i.span();
                if let Some(arg) = self.substituted(self.arg.get_expr(span), span) {
                    let ty = &const_param.ty;
                    *i = parse_quote!({
                        let #ident: #ty = #arg;
                        #i
                    });
                }
                return;
            }
        }
        visit_expr_mut(self, i)
    }

    fn visit_expr_path_mut(&mut self, i: &mut ExprPath) {
        if self.subst_in_path(&mut i.qself, &mut i.path) {
            return;
        }
        visit_expr_path_mut(self, i)
    }

    fn visit_type_mut(&mut self, i: &mut Type) {
        if let GenericParam::Type(type_param) = self.param {
            if type_is_ident(&i, &type_param.ident) {
                return self.subst(i, self.arg.get_type(i.span()));
            }
        }
        visit_type_mut(self, i)
    }

    fn visit_type_path_mut(&mut self, i: &mut TypePath) {
        if self.subst_in_path(&mut i.qself, &mut i.path) {
            return;
        }
        visit_type_path_mut(self, i)
    }

    fn visit_lifetime_param_mut(&mut self, i: &mut LifetimeParam) {
        // Need to override this to prevent `visit_lifetime_mut` from being called on the param.
        for attr in &mut i.attrs {
            self.visit_attribute_mut(attr);
        }
        for lifetime in &mut i.bounds {
            self.visit_lifetime_mut(lifetime);
        }
    }

    fn visit_generic_argument_mut(&mut self, i: &mut GenericArgument) {
        if let GenericArgument::Type(ty) = &i {
            if let GenericParam::Const(const_param) = self.param {
                if type_is_ident(ty, &const_param.ident) {
                    let span = i.span();
                    if let Some(arg) = self.substituted(self.arg.get_expr(span), span) {
                        *i = GenericArgument::Const(arg);
                    }
                    return;
                }
            }
        }
        visit_generic_argument_mut(self, i)
    }
}

impl<'a, 'b> ParamSubst<'a, 'b> {
    fn new(param: &'a GenericParam, arg: ParamSubstArg<'b>) -> Self {
        ParamSubst {
            param,
            arg,
            result: Ok(Vec::new()),
        }
    }

    fn substituted<T>(&mut self, arg: Result<Option<T>>, span: Span) -> Option<T> {
        match arg {
            Ok(arg) => {
                if let Ok(result) = &mut self.result {
                    result.push(span);
                }
                arg
            }
            Err(error) => {
                if self.result.is_ok() {
                    self.result = Err(error);
                }
                None
            }
        }
    }

    fn subst<T: ToTokens>(&mut self, node: &mut T, arg: Result<Option<T>>) {
        if let Some(arg) = self.substituted(arg, node.span()) {
            *node = arg;
        }
    }

    fn subst_with_generics(&mut self, generics: &mut Generics, f: impl FnMut(&mut ParamSubst)) {
        self.subst_with_multi_generics(iter::once(generics), f)
    }

    fn subst_with_multi_generics<'c>(
        &mut self,
        generics_iter: impl Iterator<Item = &'c mut Generics>,
        mut f: impl FnMut(&mut ParamSubst),
    ) {
        for generics in generics_iter {
            if param_generics_name_conflict(&self.param, generics) {
                // One of the generic parameters shadows `param`, so don't substitute further.
                return;
            }
            if !matches!(self.arg, ParamSubstArg::TestOnly) {
                match rename_conflicting_params(
                    generics,
                    |param| match &self.arg {
                        ParamSubstArg::TestOnly => Ok(false),
                        ParamSubstArg::Param(arg) => Ok(param_name_conflict(param, arg)),
                        ParamSubstArg::Arg(arg) => (*arg).clone().references_param(param),
                    },
                    &mut f,
                ) {
                    Ok(true) => {
                        if let Ok(result) = &mut self.result {
                            result.push(Span::call_site());
                        }
                    }
                    Ok(false) => {}
                    Err(error) => {
                        if self.result.is_ok() {
                            self.result = Err(error);
                        }
                    }
                }
            }
            self.visit_generics_mut(generics);
        }
        f(self);
    }

    fn subst_in_path(&mut self, qself: &mut Option<QSelf>, path: &mut Path) -> bool {
        if qself.is_none() && path.leading_colon.is_none() {
            if let GenericParam::Type(type_param) = self.param {
                if let Some(first) = path.segments.first_mut() {
                    if first.arguments.is_empty() && first.ident == type_param.ident {
                        let span = first.span();
                        match self.arg {
                            ParamSubstArg::TestOnly => {}
                            ParamSubstArg::Param(arg) => {
                                if let GenericParam::Type(arg) = arg {
                                    first.ident = arg.ident.clone();
                                    if first.ident != SELF_TYPE_NAME {
                                        first.ident.set_span(span);
                                    }
                                } else if self.result.is_ok() {
                                    self.result =
                                        Err(Error::new(arg.span(), "non-type arg for type param"));
                                }
                            }
                            ParamSubstArg::Arg(arg) => {
                                if let GenericArgument::Type(arg) = arg {
                                    *qself = Some(QSelf {
                                        lt_token: Default::default(),
                                        ty: Box::new(arg.clone()),
                                        position: 0,
                                        as_token: None,
                                        gt_token: Default::default(),
                                    });
                                    path.leading_colon = Some(Default::default());
                                    let old_segments =
                                        replace(&mut path.segments, Punctuated::new());
                                    for pair in old_segments.into_pairs().skip(1) {
                                        let (segment, punct) = pair.into_tuple();
                                        path.segments.push_value(segment);
                                        if let Some(punct) = punct {
                                            path.segments.push_punct(punct);
                                        }
                                    }
                                    if path.segments.is_empty() {
                                        self.result = Err(Error::new(
                                            arg.span(),
                                            "cannot replace single-segment path with type arg",
                                        ));
                                    }
                                } else if self.result.is_ok() {
                                    self.result =
                                        Err(Error::new(arg.span(), "non-type arg for type param"));
                                }
                            }
                        }
                        if let Ok(result) = &mut self.result {
                            result.push(span);
                        }
                        return true;
                    }
                }
            }
        }
        false
    }
}

impl ParamSubstArg<'_> {
    fn get_lifetime(&self, span: Span) -> Result<Option<Lifetime>> {
        match self {
            ParamSubstArg::TestOnly => Ok(None),
            ParamSubstArg::Param(arg) => {
                if let GenericParam::Lifetime(arg) = arg {
                    let mut lifetime = arg.lifetime.clone();
                    lifetime.set_span(span);
                    Ok(Some(lifetime))
                } else {
                    Err(Error::new(
                        arg.span(),
                        "non-lifetime arg for lifetime param",
                    ))
                }
            }
            ParamSubstArg::Arg(arg) => {
                if let GenericArgument::Lifetime(arg) = arg {
                    Ok(Some(arg.clone()))
                } else {
                    Err(Error::new(
                        arg.span(),
                        "non-lifetime arg for lifetime param",
                    ))
                }
            }
        }
    }

    fn get_expr(&self, span: Span) -> Result<Option<Expr>> {
        match self {
            ParamSubstArg::TestOnly => Ok(None),
            ParamSubstArg::Param(arg) => {
                if let GenericParam::Const(arg) = arg {
                    let mut ident = arg.ident.clone();
                    ident.set_span(span);
                    Ok(Some(Expr::Path(ExprPath {
                        attrs: Vec::new(),
                        qself: None,
                        path: ident.into(),
                    })))
                } else {
                    Err(Error::new(arg.span(), "non-const arg for const param"))
                }
            }
            ParamSubstArg::Arg(arg) => {
                if let GenericArgument::Const(arg) = arg {
                    Ok(Some(arg.clone()))
                } else {
                    Err(Error::new(arg.span(), "non-const arg for const param"))
                }
            }
        }
    }

    fn get_type(&self, span: Span) -> Result<Option<Type>> {
        match self {
            ParamSubstArg::TestOnly => Ok(None),
            ParamSubstArg::Param(arg) => {
                if let GenericParam::Type(arg) = arg {
                    let mut ident = arg.ident.clone();
                    ident.set_span(span);
                    Ok(Some(Type::Path(TypePath {
                        qself: None,
                        path: ident.into(),
                    })))
                } else {
                    Err(Error::new(arg.span(), "non-type arg for type param"))
                }
            }
            ParamSubstArg::Arg(arg) => {
                if let GenericArgument::Type(arg) = arg {
                    Ok(Some(arg.clone()))
                } else {
                    Err(Error::new(arg.span(), "non-type arg for type param"))
                }
            }
        }
    }
}

pub trait Substitutable: Sized {
    fn substitute_impl(&mut self, subst: &mut ParamSubst);

    fn substitute(&mut self, param: &GenericParam, arg: ParamSubstArg) -> Result<bool> {
        let mut subst = ParamSubst::new(param, arg);
        self.substitute_impl(&mut subst);
        let result = subst.result?;
        Ok(!result.is_empty())
    }

    #[allow(dead_code)]
    fn substitute_all(
        &mut self,
        generics: &Generics,
        args: &AngleBracketedGenericArguments,
    ) -> Result<bool> {
        let mut generics = generics.clone();
        let mut cloned_args = args.args.clone();
        let mut substituted = rename_conflicting_params(
            &mut generics,
            |param| {
                for arg in &mut cloned_args {
                    if arg.references_param(param)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            },
            |subst| self.substitute_impl(subst),
        )?;
        let mut args_iter = args.args.iter();
        for param in &generics.params {
            let Some(arg) = args_iter.next() else {
                return Err(Error::new(args.span(), "too few arguments"));
            };
            substituted |= self.substitute(param, ParamSubstArg::Arg(arg))?;
        }
        if let Some(arg) = args_iter.next() {
            return Err(Error::new(arg.span(), "superfluous argument"));
        }
        Ok(substituted)
    }

    fn substitute_all_params(&mut self, generics: &Generics, args: &Generics) -> Result<bool> {
        let mut generics = generics.clone();
        let mut substituted = rename_conflicting_params(
            &mut generics,
            |param| Ok(param_generics_name_conflict(param, args)),
            |subst| self.substitute_impl(subst),
        )?;
        let mut args_iter = args.params.iter();
        for param in &generics.params {
            let Some(arg) = args_iter.next() else {
                return Err(Error::new(args.span(), "too few parameters"));
            };
            substituted |= self.substitute(param, ParamSubstArg::Param(arg))?;
        }
        if let Some(arg) = args_iter.next() {
            return Err(Error::new(arg.span(), "superfluous parameter"));
        }
        Ok(substituted)
    }

    fn get_param_references(&mut self, param: &GenericParam) -> Result<Vec<Span>> {
        let mut subst = ParamSubst::new(param, ParamSubstArg::TestOnly);
        self.substitute_impl(&mut subst);
        subst.result
    }

    fn references_param(&mut self, param: &GenericParam) -> Result<bool> {
        let result = self.get_param_references(param)?;
        Ok(!result.is_empty())
    }
}

impl<E: Substitutable> Substitutable for &mut E {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        (**self).substitute_impl(subst)
    }
}

impl Substitutable for () {
    fn substitute_impl(&mut self, _subst: &mut ParamSubst) {}
}

impl<E: Substitutable, F: Substitutable> Substitutable for (E, F) {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        self.0.substitute_impl(subst);
        self.1.substitute_impl(subst);
    }
}

impl Substitutable for Block {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.visit_block_mut(self);
    }
}

impl Substitutable for Expr {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.visit_expr_mut(self);
    }
}

impl Substitutable for Type {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.visit_type_mut(self);
    }
}

impl Substitutable for Generics {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.visit_generics_mut(self);
    }
}

impl Substitutable for GenericParam {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.visit_generic_param_mut(self);
    }
}

impl Substitutable for TypeParam {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.visit_type_param_mut(self);
    }
}

impl Substitutable for PathArguments {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.visit_path_arguments_mut(self);
    }
}

impl Substitutable for GenericArgument {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.visit_generic_argument_mut(self);
    }
}

impl Substitutable for TypeParamBounds {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        for bound in self {
            subst.visit_type_param_bound_mut(bound);
        }
    }
}

impl Substitutable for Signature {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.visit_signature_mut(self);
    }
}

impl<E: Substitutable> Substitutable for TypeLevelExpr<E> {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        match self {
            TypeLevelExpr::Expr(expr) => expr.substitute_impl(subst),
            TypeLevelExpr::Match(match_expr) => match_expr.substitute_impl(subst),
        }
    }
}

impl<E: Substitutable> Substitutable for TypeLevelExprMatch<E> {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        for ty in &mut self.types {
            ty.substitute_impl(subst);
        }
        for arm in &mut self.arms {
            arm.substitute_impl(subst);
        }
    }
}

impl<E: Substitutable> Substitutable for TypeLevelArm<E> {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.subst_with_multi_generics(
            self.selectors
                .iter_mut()
                .filter_map(|variant| match variant {
                    TypeLevelArmSelector::Specific { generics, .. } => Some(generics),
                    TypeLevelArmSelector::Default { .. } => None,
                }),
            |subst| self.body.substitute_impl(subst),
        )
    }
}

impl Substitutable for ImplItem {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        match self {
            ImplItem::Const(item_const) => item_const.substitute_impl(subst),
            ImplItem::Fn(item_fn) => item_fn.substitute_impl(subst),
            ImplItem::Type(item_type) => item_type.substitute_impl(subst),
            _ => {
                subst.result = Err(Error::new(
                    self.span(),
                    "substitution not supported within this item",
                ));
            }
        }
    }
}

impl Substitutable for ImplItemConst {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.subst_with_generics(&mut self.generics, |subst| {
            subst.visit_type_mut(&mut self.ty);
            subst.visit_expr_mut(&mut self.expr);
        })
    }
}

impl Substitutable for ImplItemFn {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.subst_with_generics(&mut self.sig.generics, |subst| {
            for mut el in Punctuated::pairs_mut(&mut self.sig.inputs) {
                let it = el.value_mut();
                subst.visit_fn_arg_mut(it);
            }
            if let Some(it) = &mut self.sig.variadic {
                subst.visit_variadic_mut(it);
            }
            subst.visit_return_type_mut(&mut self.sig.output);
            subst.visit_block_mut(&mut self.block);
        })
    }
}

impl Substitutable for ImplItemType {
    fn substitute_impl(&mut self, subst: &mut ParamSubst) {
        subst.subst_with_generics(&mut self.generics, |subst| {
            subst.visit_type_mut(&mut self.ty);
        })
    }
}

pub fn param_name_conflict(param1: &GenericParam, param2: &GenericParam) -> bool {
    match (param1, param2) {
        (GenericParam::Lifetime(lifetime_param1), GenericParam::Lifetime(lifetime_param2)) => {
            lifetime_param1.lifetime == lifetime_param2.lifetime
        }
        (GenericParam::Type(type_param1), GenericParam::Type(type_param2)) => {
            type_param1.ident == type_param2.ident
        }
        (GenericParam::Type(type_param1), GenericParam::Const(const_param2)) => {
            type_param1.ident == const_param2.ident
        }
        (GenericParam::Const(const_param1), GenericParam::Type(type_param2)) => {
            const_param1.ident == type_param2.ident
        }
        (GenericParam::Const(const_param1), GenericParam::Const(const_param2)) => {
            const_param1.ident == const_param2.ident
        }
        _ => false,
    }
}

pub fn param_generics_name_conflict(param: &GenericParam, generics: &Generics) -> bool {
    generics
        .params
        .iter()
        .any(|generic_param| param_name_conflict(param, generic_param))
}

pub fn param_context_name_conflict(param: &GenericParam, mut context: &GenericsContext) -> bool {
    loop {
        match context {
            GenericsContext::Empty => return false,
            GenericsContext::WithSelf(self_param, next_context) => {
                if param_name_conflict(param, self_param) {
                    return true;
                }
                context = next_context;
            }
            GenericsContext::WithGenerics(generics, next_context) => {
                if param_generics_name_conflict(param, generics) {
                    return true;
                }
                context = next_context;
            }
        }
    }
}

pub fn rename_conflicting_params(
    generics: &mut Generics,
    mut conflicting: impl FnMut(&GenericParam) -> Result<bool>,
    mut substitute: impl FnMut(&mut ParamSubst),
) -> Result<bool> {
    let mut renamed = false;
    for param_idx in 0..generics.params.len() {
        let param = &mut generics.params[param_idx];
        if let Some(old_param) = add_underscores_if_conflicting(param, &mut conflicting)? {
            let new_param = param.clone();
            let mut subst = ParamSubst::new(&old_param, ParamSubstArg::Param(&new_param));
            subst.visit_generics_mut(generics);
            substitute(&mut subst);
            subst.result?;
            renamed = true;
        }
    }
    Ok(renamed)
}

pub fn rename_all_params(generics: &mut Generics, target_generics: &Generics) -> Result<()> {
    rename_conflicting_params(
        generics,
        |param| Ok(param_generics_name_conflict(param, target_generics)),
        |_| {},
    )?;
    let mut target_iter = target_generics.params.iter();
    for param_idx in 0..generics.params.len() {
        let param = &mut generics.params[param_idx];
        let Some(target_param) = target_iter.next() else {
            return Err(Error::new(target_generics.span(), "too few parameters"));
        };
        let old_param = param.clone();
        match param {
            GenericParam::Lifetime(lifetime_param) => {
                let GenericParam::Lifetime(target_lifetime_param) = target_param else {
                    return Err(Error::new(
                        target_param.span(),
                        "lifetime parameter expected",
                    ));
                };
                lifetime_param.lifetime.ident = target_lifetime_param.lifetime.ident.clone();
            }
            GenericParam::Type(type_param) => {
                let GenericParam::Type(target_type_param) = target_param else {
                    return Err(Error::new(target_param.span(), "type parameter expected"));
                };
                type_param.ident = target_type_param.ident.clone();
            }
            GenericParam::Const(const_param) => {
                let GenericParam::Const(target_const_param) = target_param else {
                    return Err(Error::new(target_param.span(), "const parameter expected"));
                };
                const_param.ident = target_const_param.ident.clone();
            }
        }
        let mut subst = ParamSubst::new(&old_param, ParamSubstArg::Param(target_param));
        subst.visit_generics_mut(generics);
        subst.result?;
    }
    if let Some(target_param) = target_iter.next() {
        return Err(Error::new(target_param.span(), "superfluous parameter"));
    }
    Ok(())
}

pub fn add_underscores_to_all_params(generics: &mut Generics) -> Result<bool> {
    let generics_copy = generics.clone();
    rename_conflicting_params(
        generics,
        |param| Ok(param_generics_name_conflict(param, &generics_copy)),
        |_| {},
    )
}

fn add_underscore_suffix(ident: &mut Ident) {
    *ident = ident_with_suffix(ident, "_", true)
}

fn add_underscores_if_conflicting(
    param: &mut GenericParam,
    mut conflicting: impl FnMut(&GenericParam) -> Result<bool>,
) -> Result<Option<GenericParam>> {
    if conflicting(param)? {
        let old_param = param.clone();
        loop {
            match param {
                GenericParam::Lifetime(lifetime_param) => {
                    add_underscore_suffix(&mut lifetime_param.lifetime.ident)
                }
                GenericParam::Type(type_param) => add_underscore_suffix(&mut type_param.ident),
                GenericParam::Const(const_param) => add_underscore_suffix(&mut const_param.ident),
            }
            if !conflicting(param)? {
                break;
            }
        }
        Ok(Some(old_param))
    } else {
        Ok(None)
    }
}

pub fn add_prefix_to_all_params(generics: &mut Generics, prefix: &str) -> Result<()> {
    for param_idx in 0..generics.params.len() {
        let param = &mut generics.params[param_idx];
        let old_param = param.clone();
        match param {
            GenericParam::Lifetime(lifetime_param) => {
                lifetime_param.lifetime.ident =
                    ident_with_prefix(&lifetime_param.lifetime.ident, prefix, true);
            }
            GenericParam::Type(type_param) => {
                type_param.ident = ident_with_prefix(&type_param.ident, prefix, true);
            }
            GenericParam::Const(const_param) => {
                const_param.ident = ident_with_prefix(&const_param.ident, prefix, true);
            }
        }
        let new_param = param.clone();
        let mut subst = ParamSubst::new(&old_param, ParamSubstArg::Param(&new_param));
        subst.visit_generics_mut(generics);
        subst.result?;
    }
    Ok(())
}

// Produces the data that is necessary to extract `expr` into a new object with its own generic
// parameters: For all parameters from `context` that are referenced in `expr`, adds a corresponding
// parameter to the returned `Generics` and a corresponding argument to the returned
// `PathArguments`. `expr` is modified so that references the returned parameters instead of the
// parameters from the context.
#[allow(dead_code)]
pub fn build_indirection<'a>(
    expr: &mut impl Substitutable,
    context: &'a GenericsContext<'a>,
) -> Result<Vec<(GenericParam, GenericArgument)>> {
    build_indirection_contents(&mut Vec::new(), expr, context)
}

fn build_indirection_contents<'a>(
    expr_params: &mut Vec<Vec<GenericParam>>,
    expr: &mut impl Substitutable,
    context: &'a GenericsContext<'a>,
) -> Result<Vec<(GenericParam, GenericArgument)>> {
    let (_, params) = build_indirection_impl(
        expr_params,
        expr,
        context,
        |_| Ok(()),
        |_, _, _, _| Ok(None),
        |_, _, _, _| Ok(None),
    )?;
    Ok(params)
}

fn build_indirection_impl<'a, E: Substitutable, R>(
    expr_params: &mut Vec<Vec<GenericParam>>,
    expr: &mut E,
    context: &'a GenericsContext<'a>,
    on_empty: impl FnOnce(&mut E) -> Result<R>,
    mut on_self: impl FnMut(
        &mut Vec<Vec<GenericParam>>,
        &mut E,
        &'a GenericParam,
        &'a GenericsContext<'a>,
    ) -> Result<Option<(R, Vec<(GenericParam, GenericArgument)>)>>,
    mut on_generics: impl FnMut(
        &mut Vec<Vec<GenericParam>>,
        &mut E,
        &'a Generics,
        &'a GenericsContext<'a>,
    ) -> Result<Option<(R, Vec<(GenericParam, GenericArgument)>)>>,
) -> Result<(R, Vec<(GenericParam, GenericArgument)>)> {
    match context {
        GenericsContext::Empty => {
            let result = on_empty(expr)?;
            Ok((result, Vec::new()))
        }
        GenericsContext::WithSelf(param, next_context) => {
            if let Some(result) = on_self(expr_params, expr, param, next_context)? {
                return Ok(result);
            }
            expr_params.push(vec![param.as_ref().clone()]);
            let expr_params_len = expr_params.len();
            let (result, mut params) = build_indirection_impl(
                expr_params,
                expr,
                next_context,
                on_empty,
                on_self,
                on_generics,
            )?;
            add_param_indirections(expr_params, expr, expr_params_len, &mut params)?;
            expr_params.pop();
            Ok((result, params))
        }
        GenericsContext::WithGenerics(generics, next_context) => {
            if let Some(result) = on_generics(expr_params, expr, generics, next_context)? {
                return Ok(result);
            }
            let expr_params_orig_len = expr_params.len();
            expr_params.push(generics.params.iter().map(GenericParam::clone).collect());
            let (result, mut params) = build_indirection_impl(
                expr_params,
                expr,
                next_context,
                on_empty,
                on_self,
                on_generics,
            )?;
            add_param_indirections(expr_params, expr, expr_params_orig_len, &mut params)?;
            expr_params.pop();
            Ok((result, params))
        }
    }
}

fn add_param_indirections<'a>(
    expr_params: &mut [Vec<GenericParam>],
    expr: &mut impl Substitutable,
    conflicting_expr_params: usize,
    params: &mut Vec<(GenericParam, GenericArgument)>,
) -> Result<()> {
    let indir_idx = expr_params.len() - 1;
    let mut referenced_params = Vec::new();
    for param_idx in 0..expr_params[indir_idx].len() {
        let param = &expr_params[indir_idx][param_idx];
        if expr_params[..indir_idx]
            .iter()
            .flatten()
            .any(|expr_param| param_name_conflict(param, expr_param))
        {
            // The parameter is shadowed by one that is "closer" to `expr`, so cannot be referenced
            // by `expr`.
            continue;
        }
        let param = param.clone();
        if let Some(first_ref_span) = get_first_param_reference(expr_params, expr, &param)? {
            let arg = generic_param_arg(&param, Some(first_ref_span));
            referenced_params.push((param, arg));
        }
    }
    for param_idx in 0..referenced_params.len() {
        let (param, _) = &mut referenced_params[param_idx];
        if let Some(old_param) = add_underscores_if_conflicting(param, |param| {
            Ok(expr_params[..conflicting_expr_params]
                .iter()
                .flatten()
                .any(|expr_param| param_name_conflict(param, expr_param)))
        })? {
            let new_param = param.clone();
            let mut subst = ParamSubst::new(&old_param, ParamSubstArg::Param(&new_param));
            for params in expr_params[..indir_idx].iter_mut().rev() {
                for param in params {
                    subst.visit_generic_param_mut(param);
                }
            }
            for (param, _) in &mut referenced_params {
                subst.visit_generic_param_mut(param);
            }
            expr.substitute_impl(&mut subst);
            subst.result?;
        }
    }
    params.extend(referenced_params.into_iter());
    Ok(())
}

fn get_first_param_reference(
    expr_params: &mut [Vec<GenericParam>],
    expr: &mut impl Substitutable,
    param: &GenericParam,
) -> Result<Option<Span>> {
    if let Some(first_ref_span) = expr.get_param_references(param)?.first() {
        Ok(Some(*first_ref_span))
    } else {
        for expr_params_idx in 0..expr_params.len() {
            for expr_params_inner_idx in 0..expr_params[expr_params_idx].len() {
                let expr_param = &mut expr_params[expr_params_idx][expr_params_inner_idx];
                if !param_name_conflict(expr_param, param)
                    && !expr_param.get_param_references(param)?.is_empty()
                {
                    let expr_param = expr_param.clone();
                    if let Some(first_ref_span) =
                        get_first_param_reference(expr_params, expr, &expr_param)?
                    {
                        return Ok(Some(first_ref_span));
                    }
                }
            }
        }
        Ok(None)
    }
}

// Like `build_indirection`, but additionally searches the context for a type parameter with name
// `param_ident`, replaces references to that parameter with `Self`, and returns the parameter
// separately.
// This is useful if the expression must be implemented as an associated type of a trait, which is
// the case for all match expressions.
pub fn isolate_type_param<'a>(
    expr: &mut impl Substitutable,
    context: &'a GenericsContext<'a>,
    param_ident: &Ident,
) -> Result<(&'a TypeParam, Vec<(GenericParam, GenericArgument)>)> {
    let (type_param, params) = build_indirection_impl(
        &mut Vec::new(),
        expr,
        context,
        |_| {
            Err(Error::new(
                param_ident.span(),
                format!("type param `{param_ident}` not found"),
            ))
        },
        |expr_params, expr, param, next_context| {
            match param {
                GenericParam::Type(type_param) => {
                    if &type_param.ident == param_ident {
                        expr_params.push(vec![param.clone()]);
                        let params = build_indirection_contents(expr_params, expr, next_context)?;
                        expr_params.pop();
                        return Ok(Some((type_param, params)));
                    }
                }
                _ => unreachable!(),
            }
            Ok(None)
        },
        |expr_params, expr, generics, next_context| {
            for (param_idx, param) in generics.params.iter().enumerate() {
                match param {
                    GenericParam::Lifetime(_) => {}
                    GenericParam::Type(type_param) => {
                        if &type_param.ident == param_ident {
                            let expr_params_orig_len = expr_params.len();
                            expr_params
                                .push(generics.params.iter().map(GenericParam::clone).collect());
                            let mut params =
                                build_indirection_contents(expr_params, expr, next_context)?;
                            expr_params[expr_params_orig_len].remove(param_idx);
                            add_param_indirections(
                                expr_params,
                                expr,
                                expr_params_orig_len,
                                &mut params,
                            )?;
                            let self_param = self_type_param(None, type_param.bounds.clone());
                            expr.substitute(param, ParamSubstArg::Param(&self_param))?;
                            expr_params.pop();
                            return Ok(Some((type_param, params)));
                        }
                    }
                    GenericParam::Const(const_param) => {
                        if &const_param.ident == param_ident {
                            return Err(Error::new(
                                param_ident.span(),
                                format!(
                                    "type param expected, but `{param_ident}` is a const param"
                                ),
                            ));
                        }
                    }
                }
            }
            Ok(None)
        },
    )?;
    Ok((type_param, params))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifetime_param_subst_param() {
        let param = GenericParam::Lifetime(parse_quote!('a));
        let arg = GenericParam::Lifetime(parse_quote!('x));
        let arg = ParamSubstArg::Param(&arg);
        let assert_subst_type = |mut ty: Type, result: Type| {
            ty.substitute(&param, arg).unwrap();
            assert_eq_tokens(&ty, &result);
        };
        assert_subst_type(parse_quote!(A<'a>), parse_quote!(A<'x>));
        assert_subst_type(parse_quote!(A<'b>), parse_quote!(A<'b>));
        let assert_subst_type_level_expr =
            |mut ty: TypeLevelExpr<Type>, result: TypeLevelExpr<Type>| {
                ty.substitute(&param, arg).unwrap();
                assert_eq_tokens(&ty, &result);
            };
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C => A, D => B, E => F<'b, 'a, A> }),
            parse_quote!(match <B> { C => A, D => B, E => F<'b, 'x, A> }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<'b, 'a, D> => F<'a> }),
            parse_quote!(match <B> { C<'b, 'a, D> => F<'a> }),
        );
    }

    #[test]
    fn type_param_subst_param() {
        let param = GenericParam::Type(parse_quote!(A));
        let arg = GenericParam::Type(parse_quote!(X));
        let arg = ParamSubstArg::Param(&arg);
        let assert_subst_type = |mut ty: Type, result: Type| {
            ty.substitute(&param, arg).unwrap();
            assert_eq_tokens(&ty, &result);
        };
        assert_subst_type(parse_quote!(A), parse_quote!(X));
        assert_subst_type(parse_quote!(B), parse_quote!(B));
        assert_subst_type(parse_quote!(A::C), parse_quote!(X::C));
        assert_subst_type(parse_quote!(<A>::C), parse_quote!(<X>::C));
        assert_subst_type(parse_quote!(B::C), parse_quote!(B::C));
        assert_subst_type(parse_quote!(B::A), parse_quote!(B::A));
        assert_subst_type(
            parse_quote!(F<A,B,G<A::C::D>,E>),
            parse_quote!(F<X,B,G<X::C::D>,E>),
        );
        let assert_subst_expr = |mut expr: Expr, result: Expr| {
            expr.substitute(&param, arg).unwrap();
            assert_eq_tokens(&expr, &result);
        };
        assert_subst_expr(parse_quote!(A::C), parse_quote!(X::C));
        assert_subst_expr(parse_quote!(B::C), parse_quote!(B::C));
        let assert_subst_type_level_expr =
            |mut ty: TypeLevelExpr<Type>, result: TypeLevelExpr<Type>| {
                ty.substitute(&param, arg).unwrap();
                assert_eq_tokens(&ty, &result);
            };
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C => A, D => B, E => F<A> }),
            parse_quote!(match <B> { C => X, D => B, E => F<X> }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> {
                C => match <C> {
                    D => A,
                },
            }),
            parse_quote!(match <B> {
                C => match <C> {
                    D => X,
                },
            }),
        );
        assert_subst_type_level_expr(parse_quote!(match <A> {}), parse_quote!(match <X> {}));
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, E: F<A>> => A }),
            parse_quote!(match <B> { C<D, E: F<X>> => X }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, A, E: F<A>> => A }),
            parse_quote!(match <B> { C<D, A, E: F<A>> => A }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, X> => (A, X) }),
            parse_quote!(match <B> { C<D, X_> => (X, X_) }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, X, Y: F<X>> => (A, X, Y) }),
            parse_quote!(match <B> { C<D, X_, Y: F<X_>> => (X, X_, Y) }),
        );
    }

    #[test]
    fn type_param_subst_tuple() {
        let param = GenericParam::Type(parse_quote!(A));
        let arg = GenericArgument::Type(parse_quote!((X, Y)));
        let arg = ParamSubstArg::Arg(&arg);
        let assert_subst_type = |mut ty: Type, result: Type| {
            ty.substitute(&param, arg).unwrap();
            assert_eq_tokens(&ty, &result);
        };
        assert_subst_type(parse_quote!(A), parse_quote!((X, Y)));
        assert_subst_type(parse_quote!(B), parse_quote!(B));
        assert_subst_type(parse_quote!(::A), parse_quote!(::A));
        assert_subst_type(parse_quote!(A::C), parse_quote!(<(X, Y)>::C));
        assert_subst_type(parse_quote!(<A>::C), parse_quote!(<(X, Y)>::C));
        assert_subst_type(parse_quote!(B::C), parse_quote!(B::C));
        assert_subst_type(parse_quote!(B::A), parse_quote!(B::A));
        assert_subst_type(
            parse_quote!(F<A,B,G<A::C::D>,E>),
            parse_quote!(F<(X, Y),B,G<<(X, Y)>::C::D>,E>),
        );
        let assert_subst_expr = |mut expr: Expr, result: Expr| {
            expr.substitute(&param, arg).unwrap();
            assert_eq_tokens(&expr, &result);
        };
        assert_subst_expr(parse_quote!(A::C), parse_quote!(<(X, Y)>::C));
        assert_subst_expr(parse_quote!(B::C), parse_quote!(B::C));
        let assert_subst_type_level_expr =
            |mut ty: TypeLevelExpr<Type>, result: TypeLevelExpr<Type>| {
                ty.substitute(&param, arg).unwrap();
                assert_eq_tokens(&ty, &result);
            };
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C => A, D => B, E => F<A> }),
            parse_quote!(match <B> { C => (X, Y), D => B, E => F<(X, Y)> }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> {
                C => match <C> {
                    D => A,
                },
            }),
            parse_quote!(match <B> {
                C => match <C> {
                    D => (X, Y),
                },
            }),
        );
        assert_subst_type_level_expr(parse_quote!(match <A> {}), parse_quote!(match <(X, Y)> {}));
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, E: F<A>> => A }),
            parse_quote!(match <B> { C<D, E: F<(X, Y)>> => (X, Y) }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, A, E: F<A>> => A }),
            parse_quote!(match <B> { C<D, A, E: F<A>> => A }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, X> => (A, X) }),
            parse_quote!(match <B> { C<D, X_> => ((X, Y), X_) }),
        );
    }

    #[test]
    fn expr_param_subst_param() {
        let param = GenericParam::Const(parse_quote!(const A: T));
        let arg = GenericParam::Const(parse_quote!(const X: T));
        let arg = ParamSubstArg::Param(&arg);
        let assert_subst_expr = |mut expr: Expr, result: Expr| {
            expr.substitute(&param, arg).unwrap();
            assert_eq_tokens(&expr, &result);
        };
        assert_subst_expr(parse_quote!(A), parse_quote!(X));
        assert_subst_expr(parse_quote!(B), parse_quote!(B));
        assert_subst_expr(parse_quote!(::A), parse_quote!(::A));
        assert_subst_expr(parse_quote!(A::B), parse_quote!(A::B));
        assert_subst_expr(parse_quote!(B::A), parse_quote!(B::A));
        assert_subst_expr(
            parse_quote!(f(A, B(A(C)))),
            parse_quote!({
                let A: T = X;
                f(A, B(A(C)))
            }),
        );
        assert_subst_expr(
            parse_quote!(A(|A| 2 * A)),
            parse_quote!({
                let A: T = X;
                A(|A| 2 * A)
            }),
        );
        let assert_subst_type = |mut ty: Type, result: Type| {
            ty.substitute(&param, arg).unwrap();
            assert_eq_tokens(&ty, &result);
        };
        assert_subst_type(parse_quote!(F<A>), parse_quote!(F<X>));
        let assert_subst_type_level_expr =
            |mut expr: TypeLevelExpr<Expr>, result: TypeLevelExpr<Expr>| {
                expr.substitute(&param, arg).unwrap();
                assert_eq_tokens(&expr, &result);
            };
        assert_subst_type_level_expr(
            parse_quote!(match <B> {
                C => A,
                D => B,
                E => f(A),
            }),
            parse_quote!(match <B> {
                C => X,
                D => B,
                E => {
                    let A: T = X;
                    f(A)
                },
            }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> {
                C => match <C> {
                    D => A,
                },
            }),
            parse_quote!(match <B> {
                C => match <C> {
                    D => X,
                },
            }),
        );
        assert_subst_type_level_expr(parse_quote!(match <F<A>> {}), parse_quote!(match <F<X>> {}));
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, A, E: F<A>> => f::<A> }),
            parse_quote!(match <B> { C<D, A, E: F<A>> => f::<A> }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, E: F<A>> => f::<A> }),
            parse_quote!(match <B> { C<D, E: F<X>> => f::<X> }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, X> => f::<A, X> }),
            parse_quote!(match <B> { C<D, X_> => f::<X, X_> }),
        );
    }

    #[test]
    fn expr_param_subst_op() {
        let param = GenericParam::Const(parse_quote!(const A: T));
        let arg = GenericArgument::Const(parse_quote!(X + 42));
        let arg = ParamSubstArg::Arg(&arg);
        let assert_subst_expr = |mut expr: Expr, result: Expr| {
            expr.substitute(&param, arg).unwrap();
            assert_eq_tokens(&expr, &result);
        };
        assert_subst_expr(parse_quote!(A), parse_quote!(X + 42));
        assert_subst_expr(parse_quote!(B), parse_quote!(B));
        assert_subst_expr(parse_quote!(::A), parse_quote!(::A));
        assert_subst_expr(parse_quote!(A::B), parse_quote!(A::B));
        assert_subst_expr(parse_quote!(B::A), parse_quote!(B::A));
        assert_subst_expr(
            parse_quote!(f(A, B(A(C)))),
            parse_quote!({
                let A: T = X + 42;
                f(A, B(A(C)))
            }),
        );
        assert_subst_expr(
            parse_quote!(A(|A| 2 * A)),
            parse_quote!({
                let A: T = X + 42;
                A(|A| 2 * A)
            }),
        );
        let assert_subst_type = |mut ty: Type, result: Type| {
            ty.substitute(&param, arg).unwrap();
            assert_eq_tokens(&ty, &result);
        };
        assert_subst_type(parse_quote!(F<A>), parse_quote!(F<{ X + 42 }>));
        let assert_subst_type_level_expr =
            |mut expr: TypeLevelExpr<Expr>, result: TypeLevelExpr<Expr>| {
                expr.substitute(&param, arg).unwrap();
                assert_eq_tokens(&expr, &result);
            };
        assert_subst_type_level_expr(
            parse_quote!(match <B> {
                C => A,
                D => B,
                E => f(A),
            }),
            parse_quote!(match <B> {
                C => X + 42,
                D => B,
                E => {
                    let A: T = X + 42;
                    f(A)
                },
            }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> {
                C => match <C> {
                    D => A,
                },
            }),
            parse_quote!(match <B> {
                C => match <C> {
                    D => X + 42,
                },
            }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <F<A>> {}),
            parse_quote!(match <F<{ X + 42 }>> {}),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, A, E: F<A>> => f::<A> }),
            parse_quote!(match <B> { C<D, A, E: F<A>> => f::<A> }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, E: F<A>> => f::<A> }),
            parse_quote!(match <B> { C<D, E: F<{ X + 42 }>> => f::<{ X + 42 }> }),
        );
        assert_subst_type_level_expr(
            parse_quote!(match <B> { C<D, X> => f::<A, X> }),
            parse_quote!(match <B> { C<D, X_> => f::<{ X + 42 }, X_> }),
        );
    }

    fn assert_eq_tokens<T: ToTokens>(t1: &T, t2: &T) {
        assert_eq!(
            t1.to_token_stream().to_string(),
            t2.to_token_stream().to_string(),
        );
    }
}
