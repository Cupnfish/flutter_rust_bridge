mod ty;
mod ty_boxed;
mod ty_delegate;
mod ty_enum;
mod ty_general_list;
mod ty_optional;
mod ty_primitive;
mod ty_primitive_list;
mod ty_struct;

pub use ty::*;
pub use ty_boxed::*;
pub use ty_delegate::*;
pub use ty_enum::*;
pub use ty_general_list::*;
pub use ty_optional::*;
pub use ty_primitive::*;
pub use ty_primitive_list::*;
pub use ty_struct::*;

use std::borrow::Cow;
use std::collections::HashSet;

use crate::ir::IrType::*;
use crate::ir::*;
use crate::others::*;

pub const HANDLER_NAME: &str = "FLUTTER_RUST_BRIDGE_HANDLER";

pub struct Output {
    pub code: String,
    pub extern_func_names: Vec<String>,
}

pub fn generate(api_file: &IrFile, rust_wire_mod: &str) -> Output {
    let mut generator = Generator::new();
    let code = generator.generate(api_file, rust_wire_mod);

    Output {
        code,
        extern_func_names: generator.extern_func_collector.names,
    }
}

struct Generator {
    extern_func_collector: ExternFuncCollector,
}

impl Generator {
    fn new() -> Self {
        Self {
            extern_func_collector: ExternFuncCollector::new(),
        }
    }

    fn generate(&mut self, api_file: &IrFile, rust_wire_mod: &str) -> String {
        let mut lines: Vec<String> = vec![];

        let distinct_input_types = api_file.distinct_types(true, false);
        let distinct_output_types = api_file.distinct_types(false, true);

        lines.push(r#"#![allow(non_camel_case_types, unused, clippy::redundant_closure, clippy::useless_conversion, clippy::unit_arg, non_snake_case)]"#.to_string());
        lines.push(CODE_HEADER.to_string());

        lines.push(String::new());
        lines.push(format!("use crate::{}::*;", rust_wire_mod));
        lines.push("use flutter_rust_bridge::*;".to_string());
        lines.push(String::new());

        lines.push(self.section_header_comment("imports"));
        lines.extend(self.generate_imports(
            api_file,
            rust_wire_mod,
            &distinct_input_types,
            &distinct_output_types,
        ));
        lines.push(String::new());

        lines.push(self.section_header_comment("wire functions"));
        lines.extend(api_file.funcs.iter().map(|f| self.generate_wire_func(f)));

        lines.push(self.section_header_comment("wire structs"));
        lines.extend(
            distinct_input_types
                .iter()
                .map(|ty| self.generate_wire_struct(ty, api_file)),
        );

        lines.push(self.section_header_comment("wire enums"));
        lines.extend(distinct_input_types.iter().filter_map(|ty| match ty {
            IrType::EnumRef(enu) => Some(generate_wire_enum(enu, api_file)),
            _ => None,
        }));

        lines.push(self.section_header_comment("allocate functions"));
        lines.extend(
            distinct_input_types
                .iter()
                .map(|f| self.generate_allocate_funcs(f)),
        );

        lines.push(self.section_header_comment("impl Wire2Api"));
        lines.push(self.generate_wire2api_misc().to_string());
        lines.extend(
            distinct_input_types
                .iter()
                .map(|ty| self.generate_wire2api_func(ty, api_file)),
        );

        lines.push(self.section_header_comment("impl NewWithNullPtr"));
        lines.push(self.generate_new_with_nullptr_misc().to_string());
        lines.extend(
            distinct_input_types
                .iter()
                .map(|ty| self.generate_new_with_nullptr_func(ty, api_file)),
        );

        lines.push(self.section_header_comment("impl IntoDart"));
        lines.extend(
            distinct_output_types
                .iter()
                .map(|ty| self.generate_impl_intodart(ty, api_file)),
        );

        lines.push(self.section_header_comment("executor"));
        lines.push(self.generate_executor(api_file));

        lines.push(self.section_header_comment("sync execution mode utility"));
        lines.push(self.generate_sync_execution_mode_utility());

        lines.join("\n")
    }

    fn section_header_comment(&self, section_name: &str) -> String {
        format!("// Section: {}\n", section_name)
    }

    fn generate_imports(
        &self,
        api_file: &IrFile,
        rust_wire_mod: &str,
        distinct_input_types: &[IrType],
        distinct_output_types: &[IrType],
    ) -> impl Iterator<Item = String> {
        let input_type_imports = distinct_input_types
            .iter()
            .map(|api_type| self.generate_import(api_type, api_file));
        let output_type_imports = distinct_output_types
            .iter()
            .map(|api_type| self.generate_import(api_type, api_file));

        input_type_imports
            .chain(output_type_imports)
            // Filter out `None` and unwrap
            .flatten()
            // Don't include imports from the API file
            .filter(|import| !import.starts_with(&format!("use crate::{}::", rust_wire_mod)))
            // de-duplicate
            .collect::<HashSet<String>>()
            .into_iter()
    }

    fn generate_import(&self, api_type: &IrType, api_file: &IrFile) -> Option<String> {
        TODO.imports()
    }

    fn generate_executor(&mut self, api_file: &IrFile) -> String {
        if api_file.has_executor {
            "/* nothing since executor detected */".to_string()
        } else {
            format!(
                "support::lazy_static! {{
                pub static ref {}: support::DefaultHandler = Default::default();
            }}
            ",
                HANDLER_NAME
            )
        }
    }

    fn generate_sync_execution_mode_utility(&mut self) -> String {
        self.extern_func_collector.generate(
            "free_WireSyncReturnStruct",
            &["val: support::WireSyncReturnStruct"],
            None,
            "unsafe { let _ = support::vec_from_leak_ptr(val.ptr, val.len); }",
        )
    }

    fn generate_wire_func(&mut self, func: &IrFunc) -> String {
        let params = [
            if func.mode.has_port_argument() {
                vec!["port_: i64".to_string()]
            } else {
                vec![]
            },
            func.inputs
                .iter()
                .map(|field| {
                    format!(
                        "{}: {}{}",
                        field.name.rust_style(),
                        field.ty.rust_wire_modifier(),
                        field.ty.rust_wire_type()
                    )
                })
                .collect::<Vec<_>>(),
        ]
        .concat();

        let inner_func_params = [
            match func.mode {
                IrFuncMode::Normal | IrFuncMode::Sync => vec![],
                IrFuncMode::Stream => vec!["task_callback.stream_sink()".to_string()],
            },
            func.inputs
                .iter()
                .map(|field| format!("api_{}", field.name.rust_style()))
                .collect::<Vec<_>>(),
        ]
        .concat();

        let wrap_info_obj = format!(
            "WrapInfo{{ debug_name: \"{}\", port: {}, mode: FfiCallMode::{} }}",
            func.name,
            if func.mode.has_port_argument() {
                "Some(port_)"
            } else {
                "None"
            },
            func.mode.ffi_call_mode(),
        );

        let code_wire2api = func
            .inputs
            .iter()
            .map(|field| {
                format!(
                    "let api_{} = {}.wire2api();",
                    field.name.rust_style(),
                    field.name.rust_style()
                )
            })
            .collect::<Vec<_>>()
            .join("");

        let code_call_inner_func = format!("{}({})", func.name, inner_func_params.join(", "));

        let code_call_inner_func_result = if func.fallible {
            code_call_inner_func
        } else {
            format!("Ok({})", code_call_inner_func)
        };

        let (handler_func_name, return_type, code_closure) = match func.mode {
            IrFuncMode::Sync => (
                "wrap_sync",
                Some("support::WireSyncReturnStruct"),
                format!(
                    "{}
                    {}",
                    code_wire2api, code_call_inner_func_result,
                ),
            ),
            IrFuncMode::Normal | IrFuncMode::Stream => (
                "wrap",
                None,
                format!(
                    "{}
                    move |task_callback| {}
                    ",
                    code_wire2api, code_call_inner_func_result,
                ),
            ),
        };

        self.extern_func_collector.generate(
            &func.wire_func_name(),
            &params
                .iter()
                .map(std::ops::Deref::deref)
                .collect::<Vec<_>>(),
            return_type,
            &format!(
                "
                {}.{}({}, move || {{
                    {}
                }})
                ",
                HANDLER_NAME, handler_func_name, wrap_info_obj, code_closure,
            ),
        )
    }

    fn generate_wire_struct(&mut self, ty: &IrType, api_file: &IrFile) -> String {
        // println!("generate_wire_struct: {:?}", ty);
        let fields = TODO.wire_struct_fields();

        format!(
            r###"
        #[repr(C)]
        #[derive(Clone)]
        pub struct {} {{
            {}
        }}
        "###,
            ty.rust_wire_type(),
            fields.join(",\n"),
        )
    }

    fn generate_list_allocate_func(
        &mut self,
        safe_ident: &str,
        list: &impl IrTypeTrait,
        inner: &IrType,
    ) -> String {
        self.extern_func_collector.generate(
            &format!("new_{}", safe_ident),
            &["len: i32"],
            Some(&[
                list.rust_wire_modifier().as_str(),
                list.rust_wire_type().as_str()
            ].concat()),
            &format!(
                "let wrap = {} {{ ptr: support::new_leak_vec_ptr(<{}{}>::new_with_null_ptr(), len), len }};
                support::new_leak_box_ptr(wrap)",
                list.rust_wire_type(),
                inner.rust_ptr_modifier(),
                inner.rust_wire_type()
            ),
        )
    }

    fn generate_allocate_funcs(&mut self, ty: &IrType) -> String {
        // println!("generate_allocate_funcs: {:?}", ty);
        TODO.allocate_funcs()
    }

    fn generate_wire2api_misc(&self) -> &'static str {
        r"pub trait Wire2Api<T> {
            fn wire2api(self) -> T;
        }
        
        impl<T, S> Wire2Api<Option<T>> for *mut S
            where
                *mut S: Wire2Api<T>
        {
            fn wire2api(self) -> Option<T> {
                if self.is_null() {
                    None
                } else {
                    Some(self.wire2api())
                }
            }
        }
        "
    }

    fn generate_wire2api_func(&mut self, ty: &IrType, api_file: &IrFile) -> String {
        // println!("generate_wire2api_func: {:?}", ty);
        let body = TODO.wire2api_body();

        format!(
            "impl Wire2Api<{}> for {} {{
            fn wire2api(self) -> {} {{
                {}
            }}
        }}
        ",
            ty.rust_api_type(),
            ty.rust_wire_modifier() + &ty.rust_wire_type(),
            ty.rust_api_type(),
            body,
        )
    }

    fn generate_new_with_nullptr_misc(&self) -> &'static str {
        "pub trait NewWithNullPtr {
            fn new_with_null_ptr() -> Self;
        }
        
        impl<T> NewWithNullPtr for *mut T {
            fn new_with_null_ptr() -> Self {
                std::ptr::null_mut()
            }
        }
        "
    }

    fn generate_new_with_nullptr_func(&mut self, ty: &IrType, api_file: &IrFile) -> String {
        TODO.new_with_nullptr()
    }

    fn generate_impl_intodart(&mut self, ty: &IrType, api_file: &IrFile) -> String {
        // println!("generate_impl_intodart: {:?}", ty);
        TODO.impl_intodart()
    }
}

struct ExternFuncCollector {
    names: Vec<String>,
}

impl ExternFuncCollector {
    fn new() -> Self {
        ExternFuncCollector { names: vec![] }
    }

    fn generate(
        &mut self,
        func_name: &str,
        params: &[&str],
        return_type: Option<&str>,
        body: &str,
    ) -> String {
        self.names.push(func_name.to_string());

        format!(
            r#"
                #[no_mangle]
                pub extern "C" fn {}({}) {} {{
                    {}
                }}
            "#,
            func_name,
            params.join(", "),
            return_type.map_or("".to_string(), |r| format!("-> {}", r)),
            body,
        )
    }
}