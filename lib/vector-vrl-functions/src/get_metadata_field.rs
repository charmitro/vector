use crate::{get_metadata_key, is_legacy_metadata_path, MetadataKey};
use ::value::Value;
use lookup::LookupBuf;
use vrl::prelude::*;

fn get_metadata_field(
    ctx: &mut Context,
    key: &MetadataKey,
) -> std::result::Result<Value, ExpressionError> {
    Ok(match key {
        MetadataKey::Legacy(key) => Value::from(ctx.target().get_secret(key)),
        MetadataKey::Query(query) => ctx
            .target()
            .get_metadata(query.path())?
            .unwrap_or(Value::Null),
    })
}

#[derive(Clone, Copy, Debug)]
pub struct GetMetadataField;

impl Function for GetMetadataField {
    fn identifier(&self) -> &'static str {
        "get_metadata_field"
    }

    fn parameters(&self) -> &'static [Parameter] {
        &[Parameter {
            keyword: "key",
            kind: kind::ANY,
            required: true,
        }]
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            title: "Get the datadog api key",
            source: r#"get_metadata_field(.datadog_api_key)"#,
            result: Ok("null"),
        }]
    }

    fn compile(
        &self,
        _state: (&mut state::LocalEnv, &mut state::ExternalEnv),
        _ctx: &mut FunctionCompileContext,
        mut arguments: ArgumentList,
    ) -> Compiled {
        let key = get_metadata_key(&mut arguments)?;
        Ok(Box::new(GetMetadataFieldFn { key }))
    }

    fn call_by_vm(&self, ctx: &mut Context, args: &mut VmArgumentList) -> Resolved {
        panic!("VM is being removed.")
    }
}

#[derive(Debug, Clone)]
struct GetMetadataFieldFn {
    key: MetadataKey,
}

impl Expression for GetMetadataFieldFn {
    fn resolve(&self, ctx: &mut Context) -> Resolved {
        get_metadata_field(ctx, &self.key)
    }

    fn type_def(&self, _: (&state::LocalEnv, &state::ExternalEnv)) -> TypeDef {
        match &self.key {
            MetadataKey::Legacy(_) => TypeDef::bytes().add_null().infallible(),
            MetadataKey::Query(query) => {
                // TODO: use metadata schema when it exists to return a better value here
                TypeDef::any().infallible()
            }
        }
    }
}
