use uuid::Uuid;

use super::commands::transaction;
use super::commands::{self, check};
use super::{Component, CoreError, Document, Operation, Result, Transaction};

fn parse_ids(values: &[String]) -> Result<Vec<Uuid>> {
    values
        .iter()
        .map(|value| {
            Uuid::parse_str(value)
                .map_err(|_| CoreError::invalid(format!("Invalid node ID {value}")))
        })
        .collect()
}

fn page_of(document: &Document, page_id: Option<&str>) -> Result<Uuid> {
    match page_id {
        Some(value) => Uuid::parse_str(value)
            .ok()
            .filter(|id| document.pages.iter().any(|page| page.id == *id))
            .ok_or_else(|| CoreError::invalid("Invalid page ID")),
        None => document
            .pages
            .first()
            .map(|page| page.id)
            .ok_or_else(|| CoreError::invalid("Project has no pages")),
    }
}

fn component_id(value: &str) -> Result<Uuid> {
    Uuid::parse_str(value).map_err(|_| CoreError::invalid(format!("Invalid component ID {value}")))
}

fn finish(
    document: &Document,
    mut transaction: Transaction,
    fingerprint: &str,
) -> Result<Transaction> {
    check(document, &transaction)?;
    transaction.base_fingerprint = fingerprint.to_owned();
    Ok(transaction)
}

impl crate::ProjectSession {
    fn editable_transaction(
        &self,
        build: impl FnOnce(&Document, &std::path::Path, &str) -> Result<Transaction>,
    ) -> Result<Transaction> {
        let state = self.state()?;
        state.ensure_writable()?;
        let document = state.model()?;
        let fingerprint = state.fingerprint()?;
        build(&document, &self.path, &fingerprint)
    }

    /// One transaction that wraps `node_ids` into a new group.
    pub fn group_json(&self, node_ids: Vec<String>, name: Option<String>) -> Result<Transaction> {
        let nodes = parse_ids(&node_ids)?;
        self.editable_transaction(|document, path, fingerprint| {
            let page = page_of(document, None)?;
            let transaction = commands::group(document, path, page, &nodes, name.as_deref())?;
            finish(document, transaction, fingerprint)
        })
    }

    /// One transaction that removes a group and keeps its children in place.
    pub fn ungroup_json(&self, group_id: &str) -> Result<Transaction> {
        let id = parse_ids(&[group_id.to_owned()])?[0];
        self.editable_transaction(|document, _, fingerprint| {
            let transaction = commands::ungroup(document, id)?;
            finish(document, transaction, fingerprint)
        })
    }

    /// Defines a component from `node_ids`.
    pub fn create_component_json(
        &self,
        node_ids: Vec<String>,
        name: Option<String>,
    ) -> Result<Transaction> {
        let nodes = parse_ids(&node_ids)?;
        self.editable_transaction(|document, path, fingerprint| {
            let page = page_of(document, None)?;
            let transaction =
                commands::create_component(document, path, page, &nodes, name.as_deref())?;
            finish(document, transaction, fingerprint)
        })
    }

    pub fn insert_component_instance_json(
        &self,
        component_id: &str,
        parent_id: &str,
        index: Option<u32>,
        x: f64,
        y: f64,
    ) -> Result<Transaction> {
        let component = parse_ids(&[component_id.to_owned()])?[0];
        let parent = parse_ids(&[parent_id.to_owned()])?[0];
        self.editable_transaction(|document, _, fingerprint| {
            let transaction = commands::insert_component_instance(
                document,
                component,
                parent,
                index.map(|index| index as usize),
                x,
                y,
            )?;
            finish(document, transaction, fingerprint)
        })
    }

    pub fn break_component_link_json(&self, instance_id: &str) -> Result<Transaction> {
        let id = parse_ids(&[instance_id.to_owned()])?[0];
        self.editable_transaction(|document, _, fingerprint| {
            let transaction = commands::break_component_link(document, id)?;
            finish(document, transaction, fingerprint)
        })
    }

    /// Copies a selection of this project into `parent_id` with fresh IDs.
    pub fn paste_nodes_json(
        &self,
        node_ids: Vec<String>,
        parent_id: &str,
        index: Option<u32>,
    ) -> Result<Transaction> {
        let nodes = parse_ids(&node_ids)?;
        let parent = parse_ids(&[parent_id.to_owned()])?[0];
        self.editable_transaction(|document, _, fingerprint| {
            let transaction = commands::paste(
                document,
                document,
                &nodes,
                parent,
                index.map(|index| index as usize),
            )?;
            finish(document, transaction, fingerprint)
        })
    }

    /// Copies a selection of another project into this one.
    pub fn paste_from_project_json(
        &self,
        source_path: &std::path::Path,
        node_ids: Vec<String>,
        parent_id: &str,
        index: Option<u32>,
    ) -> Result<Transaction> {
        let nodes = parse_ids(&node_ids)?;
        let parent = parse_ids(&[parent_id.to_owned()])?[0];
        let source = crate::open_project(source_path)?;
        let source = source.state()?.model()?;
        self.editable_transaction(|document, _, fingerprint| {
            let transaction = commands::paste(
                document,
                &source,
                &nodes,
                parent,
                index.map(|index| index as usize),
            )?;
            finish(document, transaction, fingerprint)
        })
    }
}

impl crate::ProjectSession {
    /// Defines a component whose root is an existing frame or group.
    pub fn set_component_json(
        &self,
        id: &str,
        root_id: &str,
        name: Option<String>,
    ) -> Result<Transaction> {
        let component = Component {
            id: component_id(id)?,
            root_id: parse_ids(&[root_id.to_owned()])?[0],
            name: name
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty()),
        };
        self.editable_transaction(|document, _, fingerprint| {
            let transaction = transaction(
                document,
                [component.root_id],
                vec![Operation::SetComponent {
                    component_id: component.id,
                    root_id: component.root_id,
                    name: component.name.clone(),
                }],
                fingerprint,
            );
            finish(document, transaction, fingerprint)
        })
    }
}
