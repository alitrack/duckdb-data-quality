//! DuckDB Data Quality Extension — expect_not_null, expect_unique, expect_in_range.
//! Uses raw DuckDB C API for in-process SQL execution, zero extra deps.

use quack_rs::prelude::*;
use quack_rs::entry_point::init_extension_v2;
use quack_rs::scalar::ScalarFunctionBuilder;

use libduckdb_sys::{
    duckdb_connection, duckdb_database, duckdb_data_chunk, duckdb_function_info,
    duckdb_vector, duckdb_extension_info, duckdb_extension_access,
    duckdb_result, DuckDBSuccess,
};
use std::sync::atomic::{AtomicPtr, Ordering};

static DB: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

fn get_con() -> Result<duckdb_connection, String> {
    let db = DB.load(Ordering::Acquire) as duckdb_database;
    if db.is_null() { return Err("db not set".into()); }
    let mut con: duckdb_connection = std::ptr::null_mut();
    if unsafe { libduckdb_sys::duckdb_connect(db, &mut con) } != DuckDBSuccess {
        return Err("connect failed".into());
    }
    Ok(con)
}

fn run_sql(sql: &str) -> Option<String> {
    use std::ffi::CString;
    let mut con = get_con().ok()?;
    let c_sql = CString::new(sql).ok()?;
    let mut result: duckdb_result = unsafe { std::mem::zeroed() };
    if unsafe { libduckdb_sys::duckdb_query(con, c_sql.as_ptr(), &mut result) } != DuckDBSuccess {
        unsafe { libduckdb_sys::duckdb_destroy_result(&mut result); }
        unsafe { libduckdb_sys::duckdb_disconnect(&mut con); }
        return None;
    }
    let val = if unsafe { libduckdb_sys::duckdb_row_count(&mut result) } > 0
        && unsafe { libduckdb_sys::duckdb_column_count(&mut result) } > 0
    {
        let p = unsafe { libduckdb_sys::duckdb_value_varchar(&mut result, 0, 0) };
        let s = unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned();
        unsafe { libduckdb_sys::duckdb_free(p as *mut _) };
        Some(s)
    } else { None };
    unsafe { libduckdb_sys::duckdb_destroy_result(&mut result); }
    unsafe { libduckdb_sys::duckdb_disconnect(&mut con); }
    val
}

// ── expect_not_null ─────────────────────────────────────────────────────

fn build_expect_not_null() -> ScalarFunctionBuilder {
    ScalarFunctionBuilder::new("expect_not_null")
        .param(TypeId::Varchar)
        .param(TypeId::Varchar)
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
    let sql = format!("SELECT count(*)::VARCHAR FROM {table} WHERE {col} IS NULL");
    let msg = match run_sql(&sql) {
        Some(ref c) if c == "0" => format!("✅ '{col}' has no NULLs in '{table}'"),
        Some(c) => format!("❌ '{col}' has {c} NULLs in '{table}'"),
        None => "❌ query failed".into(),
    };
    quack_rs::vector::VectorWriter::new(output).write_str(0, &msg);
}

// ── expect_unique ───────────────────────────────────────────────────────

fn build_expect_unique() -> ScalarFunctionBuilder {
    ScalarFunctionBuilder::new("expect_unique")
        .param(TypeId::Varchar)
        .param(TypeId::Varchar)
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
    let sql = format!(
        "SELECT count(*)::VARCHAR FROM (SELECT {col}, count(*) AS c FROM {table} GROUP BY {col} HAVING count(*) > 1)"
    );
    let msg = match run_sql(&sql) {
        Some(ref c) if c == "0" => format!("✅ '{col}' is unique in '{table}'"),
        Some(c) => format!("❌ '{col}' has {c} duplicate values in '{table}'"),
        None => "❌ query failed".into(),
    };
    quack_rs::vector::VectorWriter::new(output).write_str(0, &msg);
}

// ── expect_in_range ─────────────────────────────────────────────────────

fn build_expect_in_range() -> ScalarFunctionBuilder {
    ScalarFunctionBuilder::new("expect_in_range")
        .param(TypeId::Varchar)
        .param(TypeId::Varchar)
        .param(TypeId::Varchar)
        .param(TypeId::Varchar)
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
    let sql = format!("SELECT count(*)::VARCHAR FROM {table} WHERE {col} < {min} OR {col} > {max}");
    let msg = match run_sql(&sql) {
        Some(ref c) if c == "0" => format!("✅ all '{col}' values in [{min}, {max}] in '{table}'"),
        Some(c) => format!("❌ {c} values of '{col}' outside [{min}, {max}] in '{table}'"),
        None => "❌ query failed".into(),
    };
    quack_rs::vector::VectorWriter::new(output).write_str(0, &msg);
}

// ── Registration ────────────────────────────────────────────────────────

fn register_all(reg: &impl Registrar) -> Result<(), ExtensionError> {
    unsafe {
        reg.register_scalar(build_expect_not_null());
        reg.register_scalar(build_expect_unique());
        reg.register_scalar(build_expect_in_range());
    }
    Ok(())
}

// ── Entry ───────────────────────────────────────────────────────────────

#[no_mangle]
pub unsafe extern "C" fn data_quality_init_c_api(
    info: duckdb_extension_info,
    access: *const duckdb_extension_access,
) -> bool {
    unsafe {
        init_extension_v2(info, access, quack_rs::DUCKDB_API_VERSION, |con| {
            let db = con.as_raw_database();
            DB.store(db as *mut std::ffi::c_void, Ordering::Release);
            register_all(con)
        })
    }
}
