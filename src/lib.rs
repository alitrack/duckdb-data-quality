//! DuckDB Data Quality Extension — expect_not_null, expect_unique, expect_in_range.
//! Safe SQL generation with identifier validation and persistent connection pooling.

use quack_rs::prelude::*;
use quack_rs::entry_point::init_extension_v2;
use quack_rs::scalar::ScalarFunctionBuilder;

use libduckdb_sys::{
    duckdb_connection, duckdb_database, duckdb_data_chunk, duckdb_function_info,
    duckdb_vector, duckdb_extension_info, duckdb_extension_access,
    duckdb_result, DuckDBSuccess,
};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;
use once_cell::sync::Lazy;

// ── Persistent connection (not per-call) ──────────────────────────────

struct ConnHandle(duckdb_connection);
unsafe impl Send for ConnHandle {}
unsafe impl Sync for ConnHandle {}

static DB_HANDLE: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static CON_HANDLE: Lazy<Mutex<Option<ConnHandle>>> =
    Lazy::new(|| Mutex::new(None));

fn with_con<T>(f: impl FnOnce(duckdb_connection) -> T) -> Result<T, String> {
    let mut pool = CON_HANDLE.lock().map_err(|e| e.to_string())?;
    if pool.is_none() {
        let db = DB_HANDLE.load(Ordering::Acquire) as duckdb_database;
        if db.is_null() {
            return Err("db not initialized".into());
        }
        let mut con: duckdb_connection = std::ptr::null_mut();
        if unsafe { libduckdb_sys::duckdb_connect(db, &mut con) } != DuckDBSuccess {
            return Err("connect failed".into());
        }
        *pool = Some(ConnHandle(con));
    }
    let con = pool.as_ref().unwrap().0;
    Ok(f(con))
}

fn run_sql(sql: &str) -> String {
    use std::ffi::CString;
    let result: Result<String, String> = with_con(|con| {
        let c_sql = match CString::new(sql) {
            Ok(s) => s,
            Err(e) => return format!("Error: {e}"),
        };
        let mut result: duckdb_result = unsafe { std::mem::zeroed() };
        if unsafe { libduckdb_sys::duckdb_query(con, c_sql.as_ptr(), &mut result) } != DuckDBSuccess {
            let err = unsafe { std::ffi::CStr::from_ptr(libduckdb_sys::duckdb_result_error(&mut result)) }
                .to_string_lossy().into_owned();
            unsafe { libduckdb_sys::duckdb_destroy_result(&mut result); }
            return format!("Error: {err}");
        }
        let val = if unsafe { libduckdb_sys::duckdb_row_count(&mut result) } > 0
            && unsafe { libduckdb_sys::duckdb_column_count(&mut result) } > 0
        {
            let p = unsafe { libduckdb_sys::duckdb_value_varchar(&mut result, 0, 0) };
            if p.is_null() {
                "NULL".to_string()
            } else {
                let s = unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned();
                unsafe { libduckdb_sys::duckdb_free(p as *mut _) };
                s
            }
        } else {
            "0".to_string()
        };
        unsafe { libduckdb_sys::duckdb_destroy_result(&mut result); }
        val
    });
    result.unwrap_or_else(|e| format!("Error: {e}"))
}

// ── Safe identifier quoting ──────────────────────────────────────────

/// Validate that a table or column identifier is safe.
/// Only allow alphanumeric, underscore, and periods (for schema.table).
fn validate_identifier(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("identifier must not be empty".into());
    }
    if name.len() > 256 {
        return Err("identifier too long".into());
    }
    for ch in name.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '.' | '-' => continue,
            // Only allow safe characters
            _ => return Err(format!("invalid character in identifier: '{ch}'")),
        }
    }
    // Block dangerous patterns
    if name.contains("--") || name.contains("/*") || name.contains("*/") {
        return Err("identifier contains SQL comment markers".into());
    }
    if name.contains(';') {
        return Err("identifier contains statement separator".into());
    }
    Ok(())
}

/// Quote a validated identifier as a SQL-safe string.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('\"', "\"\""))
}

/// Build a safe `table.column` reference.
fn safe_col(table: &str, col: &str) -> Result<String, String> {
    validate_identifier(table)?;
    validate_identifier(col)?;
    Ok(format!("{}.{}", quote_ident(table), quote_ident(col)))
}

fn safe_table(table: &str) -> Result<String, String> {
    validate_identifier(table)?;
    Ok(quote_ident(table))
}

// ── expect_not_null ─────────────────────────────────────────────────

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

    let msg = match safe_col(&table, &col) {
        Ok(_) => {
            let sql = format!(
                "SELECT count(*)::VARCHAR FROM {} WHERE {} IS NULL",
                safe_table(&table).unwrap(),
                quote_ident(&col)
            );
            let cnt = run_sql(&sql);
            if cnt == "0" {
                format!("✅ '{col}' has no NULLs in '{table}'")
            } else {
                format!("❌ '{col}' has {cnt} NULLs in '{table}'")
            }
        }
        Err(e) => format!("❌ Invalid input: {e}"),
    };
    quack_rs::vector::VectorWriter::new(output).write_str(0, &msg);
}

// ── expect_unique ───────────────────────────────────────────────────

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

    let msg = match safe_col(&table, &col) {
        Ok(_) => {
            let t = safe_table(&table).unwrap();
            let c = quote_ident(&col);
            let sql = format!(
                "SELECT count(*)::VARCHAR FROM (SELECT {c}, count(*) AS c FROM {t} GROUP BY {c} HAVING count(*) > 1)"
            );
            let cnt = run_sql(&sql);
            if cnt == "0" {
                format!("✅ '{col}' is unique in '{table}'")
            } else {
                format!("❌ '{col}' has {cnt} duplicate values in '{table}'")
            }
        }
        Err(e) => format!("❌ Invalid input: {e}"),
    };
    quack_rs::vector::VectorWriter::new(output).write_str(0, &msg);
}

// ── expect_in_range ─────────────────────────────────────────────────

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

    let msg = match safe_col(&table, &col) {
        Ok(_) => {
            let t = safe_table(&table).unwrap();
            let c = quote_ident(&col);
            // Validate min/max are numeric
            let min_ok = min.parse::<f64>().is_ok() || min.chars().all(|ch| ch.is_ascii_digit() || ch == '.' || ch == '-');
            let max_ok = max.parse::<f64>().is_ok() || max.chars().all(|ch| ch.is_ascii_digit() || ch == '.' || ch == '-');
            if !min_ok || !max_ok {
                format!("❌ min/max must be numeric values")
            } else {
                let sql = format!("SELECT count(*)::VARCHAR FROM {t} WHERE {c} < {min} OR {c} > {max}");
                let cnt = run_sql(&sql);
                if cnt == "0" {
                    format!("✅ all '{col}' values in [{min}, {max}] in '{table}'")
                } else {
                    format!("❌ {cnt} values of '{col}' outside [{min}, {max}] in '{table}'")
                }
            }
        }
        Err(e) => format!("❌ Invalid input: {e}"),
    };
    quack_rs::vector::VectorWriter::new(output).write_str(0, &msg);
}

// ── Registration ────────────────────────────────────────────────────

fn register_all(reg: &impl Registrar) -> Result<(), ExtensionError> {
    unsafe {
        reg.register_scalar(build_expect_not_null());
        reg.register_scalar(build_expect_unique());
        reg.register_scalar(build_expect_in_range());
    }
    Ok(())
}

// ── Entry ───────────────────────────────────────────────────────────

#[no_mangle]
pub unsafe extern "C" fn data_quality_init_c_api(
    info: duckdb_extension_info,
    access: *const duckdb_extension_access,
) -> bool {
    unsafe {
        init_extension_v2(info, access, quack_rs::DUCKDB_API_VERSION, |con| {
            let db = con.as_raw_database();
            DB_HANDLE.store(db as *mut std::ffi::c_void, Ordering::Release);
            register_all(con)
        })
    }
}
