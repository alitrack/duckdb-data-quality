//! DuckDB Data Quality Extension — generates safe validation SQL.
//! Functions return executable SQL, not results. Zero runtime dependencies.
//!
//! Usage:
//!   SELECT expect_not_null('users', 'email') → SELECT count(*) FROM "users" WHERE "email" IS NULL
//!   SELECT expect_unique('users', 'id')      → SELECT count(*) FROM (SELECT "id" ... GROUP BY "id" HAVING count(*) > 1)

use quack_rs::prelude::*;
use quack_rs::entry_point::init_extension_v2;
use quack_rs::scalar::ScalarFunctionBuilder;

use libduckdb_sys::{
    duckdb_data_chunk, duckdb_function_info,
    duckdb_vector, duckdb_extension_info, duckdb_extension_access,
};

// ── Safe identifier ────────────────────────────────────────────────────

fn validate_ident(name: &str) -> Result<(), String> {
    if name.is_empty() { return Err("empty".into()); }
    if name.len() > 256 { return Err("too long".into()); }
    for ch in name.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '.' => continue,
            _ => return Err(format!("invalid char: '{ch}'")),
        }
    }
    if name.contains("--") || name.contains("/*") || name.contains(';') {
        return Err("dangerous pattern".into());
    }
    Ok(())
}

fn q(name: &str) -> String { format!("\"{}\"", name.replace('\"', "\"\"")) }

// ── expect_not_null ────────────────────────────────────────────────────

fn build_expect_not_null() -> ScalarFunctionBuilder {
    ScalarFunctionBuilder::new("expect_not_null")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .returns(TypeId::Varchar)
        .function(expect_not_null_fn)
}

unsafe extern "C" fn expect_not_null_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let r0 = quack_rs::vector::VectorReader::new(input, 0);
    let r1 = quack_rs::vector::VectorReader::new(input, 1);
    let table = r0.read_str(0).to_string();
    let col = r1.read_str(0).to_string();
    let out = match (validate_ident(&table), validate_ident(&col)) {
        (Ok(_), Ok(_)) => format!(
            "SELECT count(*) FROM {} WHERE {} IS NULL",
            q(&table), q(&col)
        ),
        (Err(e), _) | (_, Err(e)) => format!("-- Error: invalid identifier: {e}"),
    };
    quack_rs::vector::VectorWriter::new(output).write_str(0, &out);
}

// ── expect_unique ──────────────────────────────────────────────────────

fn build_expect_unique() -> ScalarFunctionBuilder {
    ScalarFunctionBuilder::new("expect_unique")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .returns(TypeId::Varchar)
        .function(expect_unique_fn)
}

unsafe extern "C" fn expect_unique_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let r0 = quack_rs::vector::VectorReader::new(input, 0);
    let r1 = quack_rs::vector::VectorReader::new(input, 1);
    let table = r0.read_str(0).to_string();
    let col = r1.read_str(0).to_string();
    let out = match (validate_ident(&table), validate_ident(&col)) {
        (Ok(_), Ok(_)) => {
            let t = q(&table);
            let c = q(&col);
            format!("SELECT count(*) FROM (SELECT {c}, count(*) FROM {t} GROUP BY {c} HAVING count(*) > 1)")
        }
        (Err(e), _) | (_, Err(e)) => format!("-- Error: invalid identifier: {e}"),
    };
    quack_rs::vector::VectorWriter::new(output).write_str(0, &out);
}

// ── expect_in_range ────────────────────────────────────────────────────

fn build_expect_in_range() -> ScalarFunctionBuilder {
    ScalarFunctionBuilder::new("expect_in_range")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .returns(TypeId::Varchar)
        .function(expect_in_range_fn)
}

unsafe extern "C" fn expect_in_range_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let r0 = quack_rs::vector::VectorReader::new(input, 0);
    let r1 = quack_rs::vector::VectorReader::new(input, 1);
    let r2 = quack_rs::vector::VectorReader::new(input, 2);
    let r3 = quack_rs::vector::VectorReader::new(input, 3);
    let table = r0.read_str(0).to_string();
    let col = r1.read_str(0).to_string();
    let min = r2.read_str(0).to_string();
    let max = r3.read_str(0).to_string();
    let out = match (validate_ident(&table), validate_ident(&col)) {
        (Ok(_), Ok(_)) => {
            let min_ok = min.parse::<f64>().is_ok() || min.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-');
            let max_ok = max.parse::<f64>().is_ok() || max.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-');
            if !min_ok || !max_ok {
                "-- Error: min/max must be numeric".into()
            } else {
                format!(
                    "SELECT count(*) FROM {} WHERE {} < {min} OR {} > {max}",
                    q(&table), q(&col), q(&col)
                )
            }
        }
        (Err(e), _) | (_, Err(e)) => format!("-- Error: invalid identifier: {e}"),
    };
    quack_rs::vector::VectorWriter::new(output).write_str(0, &out);
}

// ── expect_row_count ───────────────────────────────────────────────────

fn build_expect_row_count() -> ScalarFunctionBuilder {
    ScalarFunctionBuilder::new("expect_row_count")
        .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
        .returns(TypeId::Varchar)
        .function(expect_row_count_fn)
}

unsafe extern "C" fn expect_row_count_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let r0 = quack_rs::vector::VectorReader::new(input, 0);
    let r1 = quack_rs::vector::VectorReader::new(input, 1);
    let r2 = quack_rs::vector::VectorReader::new(input, 2);
    let table = r0.read_str(0).to_string();
    let op = r1.read_str(0).to_string(); // '=', '>=', '<=', '>', '<'
    let expected = r2.read_str(0).to_string();
    let out = match validate_ident(&table) {
        Ok(_) => {
            let valid_op = matches!(op.as_str(), "=" | ">=" | "<=" | ">" | "<" | "==");
            let valid_num = expected.parse::<i64>().is_ok();
            if !valid_op { "-- Error: operator must be =, >=, <=, >, <".into() }
            else if !valid_num { "-- Error: expected must be integer".into() }
            else { format!("SELECT count(*) FROM {} WHERE count(*) {} {expected}", q(&table), op.replace("==", "=")) }
        }
        Err(e) => format!("-- Error: invalid identifier: {e}"),
    };
    quack_rs::vector::VectorWriter::new(output).write_str(0, &out);
}

// ── expect_referential_integrity ─────────────────────────────────────────

fn build_expect_referential_integrity() -> ScalarFunctionBuilder {
    ScalarFunctionBuilder::new("expect_referential_integrity")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .returns(TypeId::Varchar)
        .function(expect_referential_integrity_fn)
}

unsafe extern "C" fn expect_referential_integrity_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let r0 = quack_rs::vector::VectorReader::new(input, 0);
    let r1 = quack_rs::vector::VectorReader::new(input, 1);
    let r2 = quack_rs::vector::VectorReader::new(input, 2);
    let r3 = quack_rs::vector::VectorReader::new(input, 3);
    let child_table = r0.read_str(0).to_string();
    let child_col = r1.read_str(0).to_string();
    let parent_table = r2.read_str(0).to_string();
    let parent_col = r3.read_str(0).to_string();
    let out = match (
        validate_ident(&child_table), validate_ident(&child_col),
        validate_ident(&parent_table), validate_ident(&parent_col),
    ) {
        (Ok(_), Ok(_), Ok(_), Ok(_)) => format!(
            "SELECT count(*) FROM {} c LEFT JOIN {} p ON c.{}=p.{} WHERE c.{} IS NOT NULL AND p.{} IS NULL",
            q(&child_table), q(&parent_table), q(&child_col), q(&parent_col), q(&child_col), q(&parent_col)
        ),
        _ => "-- Error: invalid identifier".into(),
    };
    quack_rs::vector::VectorWriter::new(output).write_str(0, &out);
}

// ── Registration ──────────────────────────────────────────────────────

fn register_all(reg: &impl Registrar) -> Result<(), ExtensionError> {
    unsafe {
        reg.register_scalar(build_expect_not_null());
        reg.register_scalar(build_expect_unique());
        reg.register_scalar(build_expect_in_range());
        reg.register_scalar(build_expect_row_count());
        reg.register_scalar(build_expect_referential_integrity());
    }
    Ok(())
}

// ── Entry ─────────────────────────────────────────────────────────────

#[no_mangle]
pub unsafe extern "C" fn data_quality_init_c_api(
    info: duckdb_extension_info,
    access: *const duckdb_extension_access,
) -> bool {
    unsafe {
        init_extension_v2(info, access, quack_rs::DUCKDB_API_VERSION, |con| {
            register_all(con)
        })
    }
}
