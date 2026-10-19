//! Applies the `Intent`s the panels pushed this frame.

use mmc_core::{Command, EntityEdit};

use crate::i18n::tr_args;
use crate::jobs::Done;
use crate::panels::Intent;
use crate::workspace::Tab;
use crate::{App, errors};

impl App {
    pub(crate) fn apply(&mut self, intents: Vec<Intent>) {
        let Some(map) = self.doc.as_ref().map(|d| d.map) else {
            return;
        };
        for intent in intents {
            match intent {
                Intent::Select(s) => {
                    self.selection = s;
                    self.field_edit = None;
                }
                Intent::SetField { .. } | Intent::Rename { .. } => {
                    let edit = match &intent {
                        Intent::SetField { id, key, value } => EntityEdit::SetField {
                            id: *id,
                            key: key.clone(),
                            value: value.clone(),
                        },
                        Intent::Rename { id, name } => EntityEdit::Rename {
                            id: *id,
                            name: name.clone(),
                        },
                        _ => continue,
                    };
                    let Ok(mut g) = self.project.try_lock() else {
                        self.pending.push(intent);
                        continue;
                    };
                    let r = g
                        .as_mut()
                        .map(|p| p.apply(map, Command::Entity(edit)))
                        .unwrap_or(Ok(()));
                    drop(g);
                    match r {
                        Ok(()) => self.note_command(map, false),
                        Err(e) => {
                            self.message =
                                Some(tr_args("error-edit", &[("error", &errors::text(&e))]))
                        }
                    }
                }
                Intent::OpenMap(m) => self.tabs.open(Tab::Map(m)),
                Intent::OpenPage(e) => self.tabs.open(Tab::Page(e)),
                Intent::ActivateTab(i) => {
                    if i < self.tabs.items.len() {
                        self.tabs.active = i;
                    }
                }
                Intent::CloseTab(i) => self.tabs.close(i),
                Intent::CreateChild(entity) => {
                    self.spawn("error-child", move |p| {
                        p.create_child_map(map, entity, None).map(Done::Child)
                    });
                }
                Intent::Generate(run) => {
                    let kinds = self.kinds.clone();
                    self.spawn("error-generate", move |p| {
                        let kind = p
                            .maps()
                            .iter()
                            .find(|m| m.id == map)
                            .map(|m| m.settings.kind)
                            .ok_or(mmc_core::Error::UnknownMap(map))?;
                        let k = kinds.get(kind).ok_or_else(|| {
                            mmc_core::Error::UnknownGenerator(run.generator.clone())
                        })?;
                        let cmd = k.generate(p, map, &run)?;
                        p.apply(map, cmd)?;
                        Ok(Done::Generated(map))
                    });
                }
                Intent::RegenerateSelection(run, selection) => {
                    self.spawn("error-generate", move |p| {
                        let plan = p.preview_regeneration(map, &run, Some(&selection))?;
                        Ok(Done::Preview(Box::new(plan)))
                    });
                }
                Intent::ApplyRegeneration(apply) => {
                    if let Some(plan) = self.regen.take()
                        && apply
                    {
                        let m = plan.map;
                        self.spawn("error-generate", move |p| {
                            p.apply_plan(*plan, false)?;
                            Ok(Done::Generated(m))
                        });
                    }
                }
                Intent::ToggleLayer(l) => {
                    if !self.hidden.remove(&l) {
                        self.hidden.insert(l);
                    }
                }
                Intent::SetMode(m) => self.mode = m.coerce(self.has_generators()),
            }
        }
        if let Some(c) = self.open_after
            && self.snap.maps.iter().any(|m| m.id == c)
        {
            self.open_after = None;
            self.tabs.open(Tab::Map(c));
        }
        if let Some(Tab::Map(m)) = self.tabs.active()
            && m != map
        {
            self.switch_map(m);
        }
    }
}
