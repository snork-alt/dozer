use hashbrown::HashMap;
use num_traits::FromPrimitive;
use dozer_core::errors::ExecutionError::InvalidOperation;
use dozer_types::ordered_float::OrderedFloat;
use dozer_types::rust_decimal::Decimal;
use dozer_types::tonic::codegen::Body;
use crate::pipeline::aggregation::aggregator::{Aggregator, update_map};
use crate::pipeline::errors::PipelineError;
use dozer_types::types::{Field, FieldType};
use crate::pipeline::expression::aggregate::AggregateFunctionType::Max;

pub struct MaxAggregator {
    current_state: HashMap<Field, u64>,
}

impl MaxAggregator {
    pub fn new() -> Self {
        Self {
            current_state: HashMap::new(),
        }
    }
}

impl Aggregator for MaxAggregator {
    fn update(
        &self,
        old: &Field,
        new: &Field,
        return_type: FieldType,
    ) -> Result<Field, PipelineError> {
        todo!()
    }

    fn delete(&mut self, old: &Field, return_type: FieldType) -> Result<Field, PipelineError> {
        update_map(old, 1_u64, true, &mut self.current_state);
    }

    fn insert(&mut self, new: &Field, return_type: FieldType) -> Result<Field, PipelineError> {
        update_map(new, 1_u64, true, &mut self.current_state);
    }
}

fn get_max(field_hash: &HashMap<Field, u64>, return_type: FieldType) -> Result<Field, PipelineError> {
    match return_type {
        FieldType::UInt => {
            let mut sum = 0_u64;
            let mut count = 0_u64;
            for (field, cnt) in field_hash {
                sum += field.to_uint().map_err(PipelineError::InternalExecutionError(InvalidOperation(format!("Failed to calculate average while parsing {}", field))))?;
                count += cnt;
            }
            Ok(Field::UInt(sum / count))
        }
        FieldType::Int => {
            let mut sum = 0_i64;
            let mut count = 0_i64;
            for (field, cnt) in field_hash {
                sum += field.to_int().map_err(PipelineError::InternalExecutionError(InvalidOperation(format!("Failed to calculate average while parsing {}", field))))?;
                count += cnt as i64;
            }
            Ok(Field::Int(sum / count))
        }
        FieldType::Float => {
            let mut sum = 0_f64;
            let mut count = 0_f64;
            for (field, cnt) in field_hash {
                sum += field.to_float().map_err(PipelineError::InternalExecutionError(InvalidOperation(format!("Failed to calculate average while parsing {}", field))))?;
                count += cnt as f64;
            }
            Ok(Field::Float(OrderedFloat::from(sum / count)))
        }
        FieldType::Decimal => {
            let mut sum = Decimal::from_f64(0_f64);
            let mut count = Decimal::from_f64(0_f64);
            for (field, cnt) in field_hash {
                sum += field.to_decimal().map_err(PipelineError::InternalExecutionError(InvalidOperation(format!("Failed to calculate average while parsing {}", field))))?;
                count += Decimal::from_u64(*cnt);
            }
            Ok(Field::Decimal(sum / count))
        }
        _ => Err(PipelineError::InternalExecutionError(InvalidOperation(format!("Not supported return type {} for {}", return_type, Max.to_string())))),
    }

}
