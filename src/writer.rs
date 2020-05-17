//! Helpers for writing out table rows.

use crate::{
    error::Error,
    eval::{State, Table},
    value::Value,
};
use std::{convert::TryInto, mem};

/// A generic writer which could accept rows of values.
pub trait Writer {
    /// The error type returned by the write methods.
    type Error: From<Error>;

    /// Writes a single value.
    fn write_value(&mut self, value: &Value) -> Result<(), Self::Error>;

    /// Writes the content of an INSERT statement before all rows.
    fn write_header(&mut self, qualified_table_name: &str) -> Result<(), Self::Error>;

    /// Writes the separator between the every value.
    fn write_value_separator(&mut self) -> Result<(), Self::Error>;

    /// Writes the separator between the every row.
    fn write_row_separator(&mut self) -> Result<(), Self::Error>;

    /// Writes the content of an INSERT statement after all rows.
    fn write_trailer(&mut self) -> Result<(), Self::Error>;
}

/// The state of a table within [`Env`].
#[derive(Debug)]
struct TableState<'a, W: Writer> {
    /// The parsed table.
    table: &'a Table,
    /// Writer associated with the table.
    writer: W,
    /// Records that, within an [`Env::write_row()`] call, whether this table has not been visited
    /// yet (either as a root or derived tables). This member will be reset to `true` at the start
    /// of every `Env::write_row()` call.
    fresh: bool,
    /// Records if any rows have been written out. This determines whether an INSERT statement is
    /// needed to be written or not. This member will be reset to `true` after calling
    /// [`Env::write_trailer()`].
    empty: bool,
}

/// An environment for writing rows from multiple tables generated from a single template.
#[derive(Debug)]
pub struct Env<'a, W: Writer> {
    state: &'a mut State,
    qualified: bool,
    tables: Vec<TableState<'a, W>>,
}

impl<'a, W: Writer> Env<'a, W> {
    /// Constructs a new row-writing environment.
    pub fn new(
        tables: &'a [Table],
        state: &'a mut State,
        qualified: bool,
        mut new_writer: impl FnMut(&Table) -> Result<W, W::Error>,
    ) -> Result<Self, W::Error> {
        Ok(Self {
            tables: tables
                .iter()
                .map(|table| {
                    Ok::<_, W::Error>(TableState {
                        table,
                        writer: new_writer(table)?,
                        fresh: true,
                        empty: true,
                    })
                })
                .collect::<Result<_, _>>()?,
            state,
            qualified,
        })
    }

    /// Returns an iterator of tables and writers associated with this environment.
    pub fn tables(&mut self) -> impl Iterator<Item = (&'a Table, &mut W)> + '_ {
        self.tables.iter_mut().map(|table| (table.table, &mut table.writer))
    }

    fn write_one_row(&mut self, table_index: usize) -> Result<(), W::Error> {
        let table = &mut self.tables[table_index];

        if mem::take(&mut table.empty) {
            table.writer.write_header(table.table.name.table_name(self.qualified))
        } else {
            table.writer.write_row_separator()
        }?;

        let values = table.table.row.eval(self.state)?;

        for (col_index, value) in values.iter().enumerate() {
            if col_index != 0 {
                table.writer.write_value_separator()?;
            }
            table.writer.write_value(value)?;
        }

        for (child, count) in &table.table.derived {
            let child_table = &mut self.tables[*child];
            child_table.fresh = false;

            let count =
                count
                    .eval(self.state)?
                    .try_into()
                    .map_err(|source| Error::NonIntegralGeneratedNumberOfRows {
                        table: child_table.table.name.table_name(true).to_owned(),
                        source,
                    })?;

            for r in 1..=count {
                self.state.sub_row_num = r;
                self.write_one_row(*child)?;
            }
        }

        Ok(())
    }

    /// Writes one row from each root table
    pub fn write_row(&mut self) -> Result<(), W::Error> {
        for table in &mut self.tables {
            table.fresh = true;
        }
        for i in 0..self.tables.len() {
            if mem::take(&mut self.tables[i].fresh) {
                self.state.sub_row_num = 1;
                self.write_one_row(i)?;
            }
        }
        self.state.increase_row_num();
        Ok(())
    }

    /// Concludes an INSERT statement after writing multiple rows.
    ///
    /// This method delegates to [`Writer::write_trailer()`] if any rows have been written out
    /// previously for a table. Otherwise, if no rows have been written, this method does nothing.
    pub fn write_trailer(&mut self) -> Result<(), W::Error> {
        for table in &mut self.tables {
            if !mem::replace(&mut table.empty, true) {
                table.writer.write_trailer()?;
            }
        }
        Ok(())
    }
}