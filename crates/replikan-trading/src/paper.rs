//! Deterministic snapshot-only paper venue with durable exchange-side receipts.
//! Market orders fill in full at the supplied reference. A marketable limit
//! fills at that reference; an unmarketable limit stays open until cancellation.
//! No book depth, queue, latency, real exchange or future-price fill is claimed.

use crate::execution_v2::{ExecutionEventKindV2, FillKey};
use crate::financial::{ExactOrderType, SignedAmount};
use crate::orders::Side;
use crate::{Config, Error, Intent, Observation, Result, VenueAdapter, authorize, connection};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::Path;

pub struct PaperVenue {
    connection: Connection,
    config: Config,
}

impl PaperVenue {
    pub fn open(path: impl AsRef<Path>, config: Config) -> Result<Self> {
        config.validate()?;
        let mut connection = connection(path.as_ref())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch("CREATE TABLE IF NOT EXISTS paper_config (id INTEGER PRIMARY KEY CHECK(id=1), json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS paper_orders (client_id TEXT PRIMARY KEY, intent TEXT NOT NULL, receipts TEXT NOT NULL);")?;
        let json = serde_json::to_string(&config)?;
        transaction.execute(
            "INSERT OR IGNORE INTO paper_config(id,json) VALUES (1,?1)",
            [&json],
        )?;
        let stored: String =
            transaction.query_row("SELECT json FROM paper_config WHERE id=1", [], |row| {
                row.get(0)
            })?;
        if stored != json {
            return Err(Error("paper venue configuration mismatch".into()));
        }
        transaction.commit()?;
        Ok(Self { connection, config })
    }
}

impl VenueAdapter for PaperVenue {
    fn identity(&self) -> (&str, &str) {
        (&self.config.venue, &self.config.account_id)
    }

    fn submit(
        &mut self,
        intent: &Intent,
        config: &Config,
        now_ms: i64,
    ) -> Result<Vec<Observation>> {
        if config != &self.config {
            return Err(Error("paper configuration mismatch".into()));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let encoded = serde_json::to_string(intent)?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT intent,receipts FROM paper_orders WHERE client_id=?1",
                [&intent.client_order_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((original, receipts)) = existing {
            if original != encoded {
                return Err(Error("paper client identity conflict".into()));
            }
            return Ok(serde_json::from_str(&receipts)?);
        }
        authorize(intent, config, now_ms)?;
        let marketable = match intent.request.order_type {
            ExactOrderType::Market => true,
            ExactOrderType::Limit { price } => match intent.request.side {
                Side::Buy => price >= intent.reference.price,
                Side::Sell => price <= intent.reference.price,
            },
            _ => return Err(Error("unsupported paper order type".into())),
        };
        let mut receipts = vec![Observation {
            native_sequence: None,
            received_at_ms: now_ms,
            kind: ExecutionEventKindV2::Accepted {
                exchange_order_id: format!("paper:{}", intent.client_order_id),
            },
        }];
        if marketable {
            let rules = config
                .instruments
                .get(&intent.request.instrument_id)
                .ok_or_else(|| Error("missing instrument".into()))?;
            receipts.push(Observation {
                native_sequence: None,
                received_at_ms: now_ms,
                kind: ExecutionEventKindV2::Fill {
                    key: FillKey {
                        venue: config.venue.clone(),
                        account_id: config.account_id.clone(),
                        instrument_id: intent.request.instrument_id.clone(),
                        trade_id: format!("paper-fill:{}", intent.client_order_id),
                    },
                    occurred_at_ms: now_ms,
                    price: intent.reference.price,
                    quantity: intent.request.quantity,
                    fee_asset: rules.quote_asset.clone(),
                    fee_amount: SignedAmount::from(config.paper_quote_fee),
                },
            });
        }
        transaction.execute(
            "INSERT INTO paper_orders(client_id,intent,receipts) VALUES (?1,?2,?3)",
            params![
                intent.client_order_id,
                encoded,
                serde_json::to_string(&receipts)?
            ],
        )?;
        transaction.commit()?;
        Ok(receipts)
    }

    fn query(&mut self, id: &str, _now_ms: i64) -> Result<Option<Vec<Observation>>> {
        let json: Option<String> = self
            .connection
            .query_row(
                "SELECT receipts FROM paper_orders WHERE client_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(Error::from))
            .transpose()
    }

    fn cancel(&mut self, id: &str, now_ms: i64) -> Result<Vec<Observation>> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let json: String = transaction.query_row(
            "SELECT receipts FROM paper_orders WHERE client_id=?1",
            [id],
            |row| row.get(0),
        )?;
        let mut receipts: Vec<Observation> = serde_json::from_str(&json)?;
        if receipts
            .iter()
            .any(|observation| matches!(observation.kind, ExecutionEventKindV2::Fill { .. }))
        {
            return Ok(vec![Observation {
                native_sequence: None,
                received_at_ms: now_ms,
                kind: ExecutionEventKindV2::CancelRejected {
                    reason: "paper order already filled".into(),
                },
            }]);
        }
        if !receipts
            .iter()
            .any(|observation| matches!(observation.kind, ExecutionEventKindV2::Canceled { .. }))
        {
            receipts.push(Observation {
                native_sequence: None,
                received_at_ms: now_ms,
                kind: ExecutionEventKindV2::Canceled {
                    effective_at_ms: now_ms,
                },
            });
            transaction.execute(
                "UPDATE paper_orders SET receipts=?1 WHERE client_id=?2",
                params![serde_json::to_string(&receipts)?, id],
            )?;
        }
        transaction.commit()?;
        Ok(receipts)
    }
}
