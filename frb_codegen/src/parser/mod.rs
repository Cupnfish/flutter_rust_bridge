mod ty;

use std::collections::{HashMap, HashSet};
use std::string::String;

use lazy_static::lazy_static;
use log::debug;
use quote::quote;
use regex::Regex;
use syn::*;

use crate::ir::IrType::*;
use crate::ir::*;

use crate::generator::rust::HANDLER_NAME;
use crate::source_graph::{Crate, Enum, Struct};

lazy_static! {
    static ref CAPTURE_RESULT: GenericCapture = GenericCapture::new("Result");
}

pub fn parse(source_rust_content: &str, file: File, manifest_path: &str) -> IrFile {
    let crate_map = Crate::new(manifest_path);

    let src_fns = extract_fns_from_file(&file);

    let mut src_structs = HashMap::new();
    crate_map.root_module.collect_structs(&mut src_structs);

    let mut src_enums = HashMap::new();
    crate_map.root_module.collect_enums(&mut src_enums);

    let parser = Parser {
        src_structs,
        src_enums,
        struct_pool: HashMap::new(),
        enum_pool: HashMap::new(),
        parsing_or_parsed_struct_names: HashSet::new(),
        parsed_enums: HashSet::new(),
    };
    parser.parse(source_rust_content, src_fns)
}

struct Parser<'a> {
    src_structs: HashMap<String, &'a Struct>,
    parsing_or_parsed_struct_names: HashSet<String>,
    struct_pool: IrStructPool,

    src_enums: HashMap<String, &'a Enum>,
    parsed_enums: HashSet<String>,
    enum_pool: IrEnumPool,
}

fn extract_comments(attrs: &[Attribute]) -> Vec<IrComment> {
    attrs
        .iter()
        .filter_map(|attr| match attr.parse_meta() {
            Ok(Meta::NameValue(MetaNameValue {
                path,
                lit: Lit::Str(lit),
                ..
            })) if path.is_ident("doc") => Some(IrComment::from(lit.value().as_ref())),
            _ => None,
        })
        .collect()
}

impl<'a> Parser<'a> {
    fn parse(mut self, source_rust_content: &str, src_fns: Vec<&ItemFn>) -> IrFile {
        let funcs = src_fns.iter().map(|f| self.parse_function(f)).collect();

        let has_executor = source_rust_content.contains(HANDLER_NAME);

        IrFile {
            funcs,
            struct_pool: self.struct_pool,
            enum_pool: self.enum_pool,
            has_executor,
        }
    }

    fn parse_function(&mut self, func: &ItemFn) -> IrFunc {
        debug!("parse_function function name: {:?}", func.sig.ident);

        let sig = &func.sig;
        let func_name = sig.ident.to_string();

        let mut inputs = Vec::new();
        let mut output = None;
        let mut mode = None;
        let mut fallible = true;

        for sig_input in &sig.inputs {
            if let FnArg::Typed(ref pat_type) = sig_input {
                let name = if let Pat::Ident(ref pat_ident) = *pat_type.pat {
                    format!("{}", pat_ident.ident)
                } else {
                    panic!("unexpected pat_type={:?}", pat_type)
                };
                let type_string = type_to_string(&pat_type.ty);

                if let Some(stream_sink_inner_type) = self.try_parse_stream_sink(&type_string) {
                    output = Some(stream_sink_inner_type);
                    mode = Some(IrFuncMode::Stream);
                } else {
                    inputs.push(IrField {
                        name: IrIdent::new(name),
                        ty: self.parse_type(&type_string),
                        comments: extract_comments(&pat_type.attrs),
                    });
                }
            } else {
                panic!("unexpected sig_input={:?}", sig_input);
            }
        }

        if output.is_none() {
            output = Some(match &sig.output {
                ReturnType::Type(_, ty) => {
                    let type_string = type_to_string(ty);
                    if let Some(inner) = CAPTURE_RESULT.captures(&type_string) {
                        self.parse_type(&inner)
                    } else {
                        fallible = false;
                        self.parse_type(&type_string)
                    }
                }
                ReturnType::Default => {
                    fallible = false;
                    IrType::Primitive(IrTypePrimitive::Unit)
                }
            });
            mode = Some(
                if let Some(IrType::Delegate(IrTypeDelegate::SyncReturnVecU8)) = output {
                    IrFuncMode::Sync
                } else {
                    IrFuncMode::Normal
                },
            );
        }

        // let comments = func.attrs.iter().filter_map(extract_comments).collect();

        IrFunc {
            name: func_name,
            inputs,
            output: output.expect("unsupported output"),
            fallible,
            mode: mode.expect("unsupported mode"),
            comments: extract_comments(&func.attrs),
        }
    }

    fn try_parse_stream_sink(&mut self, ty: &str) -> Option<IrType> {
        CAPTURE_STREAM_SINK
            .captures(ty)
            .map(|inner| self.parse_type(&inner))
    }
}

fn extract_fns_from_file(file: &File) -> Vec<&ItemFn> {
    let mut src_fns = Vec::new();

    for item in file.items.iter() {
        if let Item::Fn(ref item_fn) = item {
            if let Visibility::Public(_) = &item_fn.vis {
                src_fns.push(item_fn);
            }
        }
    }

    src_fns
}

/// syn -> string https://github.com/dtolnay/syn/issues/294
fn type_to_string(ty: &Type) -> String {
    quote!(#ty).to_string().replace(' ', "")
}

struct GenericCapture {
    regex: Regex,
}

impl GenericCapture {
    pub fn new(cls_name: &str) -> Self {
        let regex =
            Regex::new(&*format!("^[^<]*{}<([a-zA-Z0-9_<>()\\[\\];]+)>$", cls_name)).unwrap();
        Self { regex }
    }

    /// e.g. List<Tom> => return Some(Tom)
    pub fn captures(&self, s: &str) -> Option<String> {
        self.regex
            .captures(s)
            .iter()
            .find_map(|capture| capture.get(1))
            .map(|inner| inner.as_str().to_owned())
    }
}