//! Python bindings: `import netcfg`.
//!
//! ```python
//! e = netcfg.Engine("templates/nxos")   # dialect from the templates' declaration
//! result = e.parse("Device", text)       # result.value is a dict, result.unmanaged a list of paths
//! text = e.render("Device", result.value)
//! schema = e.schema("Device")
//! ```

use netcfg_core::{Engine as Core, Value};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyList};

fn to_py(py: Python<'_>, v: &Value) -> PyResult<PyObject> {
    Ok(match v {
        Value::Null => py.None(),
        Value::Bool(b) => b.into_py(py),
        Value::Int(i) => i.into_py(py),
        Value::Str(s) => s.into_py(py),
        Value::List(l) => {
            let items = l.iter().map(|x| to_py(py, x)).collect::<PyResult<Vec<_>>>()?;
            PyList::new_bound(py, items).into_py(py)
        }
        Value::Record(r) => {
            let d = PyDict::new_bound(py);
            for (k, v) in r {
                d.set_item(k, to_py(py, v)?)?;
            }
            d.into_py(py)
        }
    })
}

fn from_py(v: &Bound<'_, PyAny>) -> PyResult<Value> {
    if v.is_none() {
        return Ok(Value::Null);
    }
    if let Ok(b) = v.downcast::<PyBool>() {
        return Ok(Value::Bool(b.is_true()));
    }
    if let Ok(i) = v.extract::<i64>() {
        return Ok(Value::Int(i));
    }
    if let Ok(s) = v.extract::<String>() {
        return Ok(Value::Str(s));
    }
    if let Ok(d) = v.downcast::<PyDict>() {
        let mut r = netcfg_core::Record::new();
        for (k, x) in d.iter() {
            r.insert(k.extract::<String>()?, from_py(&x)?);
        }
        return Ok(Value::Record(r));
    }
    if let Ok(l) = v.downcast::<PyList>() {
        return Ok(Value::List(l.iter().map(|x| from_py(&x)).collect::<PyResult<_>>()?));
    }
    Err(PyTypeError::new_err(format!("unsupported value: {}", v.get_type().name()?)))
}

/// The result of parsing: the model value and the config the model doesn't cover.
#[pyclass]
struct Parsed {
    #[pyo3(get)]
    value: PyObject,
    /// Unmanaged lines as paths, e.g. `router bgp 65000 > neighbor 10.0.0.1 > bfd`.
    #[pyo3(get)]
    unmanaged: Vec<String>,
}

#[pyclass]
struct Engine {
    core: Core,
}

#[pymethods]
impl Engine {
    /// Load every `.ttp` file under `templates`. `dialect` names a builtin (ios, nxos, eos,
    /// junos) used only when the templates don't declare their own.
    #[new]
    #[pyo3(signature = (templates, dialect = None))]
    fn new(templates: &str, dialect: Option<&str>) -> PyResult<Self> {
        let core = Core::load_dir(std::path::Path::new(templates), dialect).map_err(|e| PyValueError::new_err(e.0))?;
        Ok(Engine { core })
    }

    /// Build an engine from template text instead of a directory.
    #[staticmethod]
    #[pyo3(signature = (text, dialect = None))]
    fn from_text(text: &str, dialect: Option<&str>) -> PyResult<Self> {
        let core = Core::from_text("<text>", text, dialect).map_err(|e| PyValueError::new_err(e.0))?;
        Ok(Engine { core })
    }

    #[getter]
    fn models(&self) -> Vec<String> {
        self.core.model_names().into_iter().map(String::from).collect()
    }

    #[getter]
    fn dialect(&self) -> String {
        self.core.dialect.name.clone()
    }

    /// Parse config text into model data. Raises ValueError on unrepresentable config.
    fn parse(&self, py: Python<'_>, model: &str, text: &str) -> PyResult<Parsed> {
        let parsed = py.allow_threads(|| self.core.parse(model, text)).map_err(|e| PyValueError::new_err(e.0))?;
        Ok(Parsed { value: to_py(py, &parsed.value)?, unmanaged: parsed.unmanaged_paths() })
    }

    /// Render model data (dict) to config text.
    fn render(&self, py: Python<'_>, model: &str, value: &Bound<'_, PyAny>) -> PyResult<String> {
        let v = from_py(value)?;
        py.allow_threads(|| self.core.render(model, &v)).map_err(|e| PyValueError::new_err(e.0))
    }

    /// How every field of a model is spelled in config (the `netcfg explain` table).
    fn explain(&self, model: &str) -> PyResult<String> {
        self.core.explain(model).map_err(|e| PyValueError::new_err(e.0))
    }

    /// JSON Schema (as a dict) for a model's data.
    fn schema(&self, py: Python<'_>, model: &str) -> PyResult<PyObject> {
        let s = self.core.schema(model).map_err(|e| PyValueError::new_err(e.0))?;
        to_py(py, &Value::from_json(&s))
    }
}

#[pymodule]
fn netcfg(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Engine>()?;
    m.add_class::<Parsed>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("__build__", concat!(env!("CARGO_PKG_VERSION"), "+", env!("NETCFG_BUILD")))?;
    Ok(())
}
