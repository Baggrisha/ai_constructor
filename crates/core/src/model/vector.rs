use super::domain::{VectorCommand, VectorData, VectorShape};
use super::{CoreError, Result};

type Point = (f64, f64);

// A coordinate can lie between adjacent f64 values, e.g. 2^53 + 1.
// Keep the rounding residue until bounds have been subtracted.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
struct Coordinate {
    high: f64,
    low: f64,
}

impl Coordinate {
    fn value(value: f64) -> Self {
        Self {
            high: value,
            low: 0.0,
        }
    }

    fn sum(a: f64, b: f64) -> Result<Self> {
        let high = a + b;
        if !a.is_finite() || !b.is_finite() || !high.is_finite() {
            return Err(CoreError::invalid("Vector bounds overflow"));
        }
        let b_virtual = high - a;
        let low = (a - (high - b_virtual)) + (b - b_virtual);
        if !low.is_finite() {
            return Err(CoreError::invalid("Vector bounds overflow"));
        }
        Ok(Self { high, low })
    }

    fn add(self, value: f64) -> Result<Self> {
        let sum = Self::sum(self.high, value)?;
        let correction = Self::sum(self.low, sum.low)?;
        let normalized = Self::sum(sum.high, correction.high)?;
        let residual = Self::sum(normalized.low, correction.low)?;
        if residual.low != 0.0 {
            return Err(CoreError::invalid("Vector bounds exceed numeric precision"));
        }
        Self::sum(normalized.high, residual.high)
    }

    fn difference(self, min: Self) -> Result<f64> {
        let difference = Self::sum(self.high, -min.high)?
            .add(self.low)?
            .add(-min.low)?;
        let size = difference.high + difference.low;
        if !size.is_finite() {
            return Err(CoreError::invalid("Vector bounds overflow"));
        }
        Ok(size)
    }
}

#[derive(Default)]
struct Bounds(Option<(Coordinate, Coordinate, Coordinate, Coordinate)>);

impl Bounds {
    fn include(&mut self, (x, y): Point) -> Result<()> {
        if !x.is_finite() || !y.is_finite() {
            return Err(CoreError::invalid("Vector bounds overflow"));
        }
        self.include_exact(Coordinate::value(x), Coordinate::value(y));
        Ok(())
    }

    fn include_exact(&mut self, x: Coordinate, y: Coordinate) {
        self.0 = Some(match self.0 {
            Some((min_x, min_y, max_x, max_y)) => (
                if x < min_x { x } else { min_x },
                if y < min_y { y } else { min_y },
                if x > max_x { x } else { max_x },
                if y > max_y { y } else { max_y },
            ),
            None => (x, y, x, y),
        });
    }

    fn segment(&mut self, from: Point, to: Point) -> Result<()> {
        self.include(from)?;
        self.include(to)
    }

    fn curve(&mut self, from: Point, first: Point, second: Point, to: Point) -> Result<()> {
        self.segment(from, to)?;
        let x = curve_offsets(from.0, first.0, second.0, to.0)?;
        let y = curve_offsets(from.1, first.1, second.1, to.1)?;
        for t in extrema(from.0, first.0, second.0, to.0)?
            .into_iter()
            .chain(extrema(from.1, first.1, second.1, to.1)?)
            .flatten()
        {
            self.include_exact(
                curve_coordinate(from.0, &x, t)?,
                curve_coordinate(from.1, &y, t)?,
            );
        }
        Ok(())
    }
}

// The derivative is quadratic. Normalize its control-point differences first
// and use the stable quadratic formula so distant control points are not
// mistaken for actual curve bounds.
fn extrema(p0: f64, p1: f64, p2: f64, p3: f64) -> Result<[Option<f64>; 2]> {
    let points = [p0, p1, p2, p3];
    let differences = [p1 - p0, p2 - p1, p3 - p2];
    let scale = if differences.iter().all(|value| value.is_finite()) {
        differences
            .iter()
            .map(|value| value.abs())
            .fold(0.0, f64::max)
    } else {
        points.iter().map(|value| value.abs()).fold(0.0, f64::max)
    };
    if scale == 0.0 {
        return Ok([None, None]);
    }
    let mut normalized = [0.0; 3];
    for (index, difference) in differences.into_iter().enumerate() {
        normalized[index] = if difference.is_finite() {
            difference / scale
        } else {
            points[index + 1] / scale - points[index] / scale
        };
        if (normalized[index] == 0.0 && points[index] != points[index + 1])
            || normalized[index].is_subnormal()
        {
            return Err(CoreError::invalid("Vector curve exceeds numeric precision"));
        }
    }
    let [d0, d1, d2] = normalized;
    let a = d0 - 2.0 * d1 + d2;
    let b = 2.0 * (d1 - d0);
    let c = d0;
    let root = |value: f64| (value.is_finite() && value > 0.0 && value < 1.0).then_some(value);
    if a == 0.0 {
        return Ok([if b == 0.0 { None } else { root(-c / b) }, None]);
    }
    let discriminant = b.mul_add(b, -4.0 * a * c);
    if discriminant < 0.0 {
        return Ok([None, None]);
    }
    let q = -0.5 * (b + discriminant.sqrt().copysign(b));
    if q == 0.0 {
        Ok([root(-b / (2.0 * a)), None])
    } else {
        Ok([root(q / a), root(c / q)])
    }
}

fn curve_offsets(origin: f64, first: f64, second: f64, end: f64) -> Result<[Coordinate; 4]> {
    Ok([
        Coordinate::value(0.0),
        Coordinate::sum(first, -origin)?,
        Coordinate::sum(second, -origin)?,
        Coordinate::sum(end, -origin)?,
    ])
}

fn curve_coordinate(origin: f64, offsets: &[Coordinate; 4], t: f64) -> Result<Coordinate> {
    let scale = offsets
        .iter()
        .flat_map(|point| [point.high.abs(), point.low.abs()])
        .fold(0.0, f64::max);
    if scale == 0.0 {
        return Ok(Coordinate::value(origin));
    }
    let mut high = [0.0; 4];
    let mut low = [0.0; 4];
    for (index, point) in offsets.iter().enumerate() {
        high[index] = point.high / scale;
        low[index] = point.low / scale;
        if (point.high != 0.0 && high[index] == 0.0)
            || (point.low != 0.0 && low[index] == 0.0)
            || high[index].is_subnormal()
            || low[index].is_subnormal()
        {
            return Err(CoreError::invalid("Vector curve exceeds numeric precision"));
        }
    }
    let high = cubic(high, t)?;
    let low = cubic(low, t)?;
    let scaled_high = high * scale;
    let scaled_low = low * scale;
    if (high != 0.0 && scaled_high == 0.0) || (low != 0.0 && scaled_low == 0.0) {
        return Err(CoreError::invalid("Vector curve exceeds numeric precision"));
    }
    Coordinate::value(origin).add(scaled_high)?.add(scaled_low)
}

fn cubic([p0, p1, p2, p3]: [f64; 4], t: f64) -> Result<f64> {
    let mix = |a: f64, b: f64| -> Result<f64> {
        let left = a * (1.0 - t);
        let right = b * t;
        let result = left + right;
        if (a != 0.0 && left == 0.0)
            || (b != 0.0 && right == 0.0)
            || left.is_subnormal()
            || right.is_subnormal()
            || result.is_subnormal()
        {
            return Err(CoreError::invalid("Vector curve exceeds numeric precision"));
        }
        Ok(result)
    };
    let a = mix(p0, p1)?;
    let b = mix(p1, p2)?;
    let c = mix(p2, p3)?;
    mix(mix(a, b)?, mix(b, c)?)
}

pub(super) fn intrinsic_size(vector: &VectorData) -> Result<(f64, f64)> {
    let mut bounds = Bounds::default();
    for shape in &vector.shapes {
        match shape {
            VectorShape::Rect { x, y, w, h, .. } => {
                bounds.include((*x, *y))?;
                bounds.include_exact(Coordinate::sum(*x, *w)?, Coordinate::sum(*y, *h)?);
            }
            VectorShape::Circle { cx, cy, r } => {
                bounds.include_exact(Coordinate::sum(*cx, -*r)?, Coordinate::sum(*cy, -*r)?);
                bounds.include_exact(Coordinate::sum(*cx, *r)?, Coordinate::sum(*cy, *r)?);
            }
            VectorShape::Ellipse { cx, cy, rx, ry } => {
                bounds.include_exact(Coordinate::sum(*cx, -*rx)?, Coordinate::sum(*cy, -*ry)?);
                bounds.include_exact(Coordinate::sum(*cx, *rx)?, Coordinate::sum(*cy, *ry)?);
            }
            VectorShape::Line { x1, y1, x2, y2 } => bounds.segment((*x1, *y1), (*x2, *y2))?,
            VectorShape::Polyline { points } | VectorShape::Polygon { points } => {
                for point in points {
                    bounds.include((point.x, point.y))?;
                }
            }
        }
    }
    for path in &vector.paths {
        let mut current = None;
        let mut start = None;
        let mut drawn = false;
        for command in path {
            match command {
                VectorCommand::MoveTo { x, y } => {
                    current = Some((*x, *y));
                    start = current;
                    drawn = false;
                }
                VectorCommand::LineTo { x, y } => {
                    let from = current
                        .ok_or_else(|| CoreError::unsupported("Vector path has no initial move"))?;
                    let to = (*x, *y);
                    bounds.segment(from, to)?;
                    current = Some(to);
                    drawn = true;
                }
                VectorCommand::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x,
                    y,
                } => {
                    let from = current
                        .ok_or_else(|| CoreError::unsupported("Vector path has no initial move"))?;
                    let to = (*x, *y);
                    bounds.curve(from, (*x1, *y1), (*x2, *y2), to)?;
                    current = Some(to);
                    drawn = true;
                }
                VectorCommand::Close {} => {
                    let from = current
                        .ok_or_else(|| CoreError::unsupported("Vector path has no initial move"))?;
                    let to = start.expect("a current point requires a subpath start");
                    if drawn {
                        bounds.segment(from, to)?;
                    }
                    current = Some(to);
                }
            }
        }
    }
    let (min_x, min_y, max_x, max_y) = bounds
        .0
        .ok_or_else(|| CoreError::unsupported("An empty vector has no intrinsic geometry"))?;
    Ok((max_x.difference(min_x)?, max_y.difference(min_y)?))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{intrinsic_size, VectorData};

    #[test]
    fn curve_extrema_and_mixed_geometry_are_measured_without_control_bounds() {
        let vector: VectorData = serde_json::from_value(json!({
            "paths": [[
                {"type":"moveTo","x":0,"y":0},
                {"type":"curveTo","x1":0,"y1":3,"x2":3,"y2":3,"x":3,"y":0}
            ]]
        }))
        .unwrap();
        assert_eq!(intrinsic_size(&vector).unwrap(), (3.0, 2.25));

        let vector: VectorData = serde_json::from_value(json!({
            "paths": [[
                {"type":"moveTo","x":-10,"y":-5},
                {"type":"lineTo","x":10,"y":15},
                {"type":"close"}
            ]],
            "shapes": [{"type":"rect","x":100,"y":10,"w":10,"h":10}]
        }))
        .unwrap();
        assert_eq!(intrinsic_size(&vector).unwrap(), (120.0, 25.0));

        let vector: VectorData = serde_json::from_value(json!({
            "paths": [],
            "shapes": [{"type":"line","x1":0,"y1":0,"x2":2,"y2":0}]
        }))
        .unwrap();
        assert_eq!(intrinsic_size(&vector).unwrap(), (2.0, 0.0));
    }

    #[test]
    fn large_curve_does_not_overflow_when_its_bounds_are_finite() {
        let vector: VectorData = serde_json::from_value(json!({
            "paths": [[
                {"type":"moveTo","x":0,"y":0},
                {"type":"curveTo","x1":1e308,"y1":0,"x2":-1e308,"y2":0,"x":0,"y":0}
            ]]
        }))
        .unwrap();
        let (width, height) = intrinsic_size(&vector).unwrap();
        let expected = 1e308 / 3.0_f64.sqrt();
        assert!((width / expected - 1.0).abs() < 1e-14);
        assert_eq!(height, 0.0);
    }

    #[test]
    fn bounds_preserve_small_geometry_at_large_coordinates() {
        let base = 9_007_199_254_740_992.0_f64;
        for (shape, expected) in [
            (
                json!({"type":"rect","x":0.1,"y":0,"w":0.3,"h":1}),
                (0.3, 1.0),
            ),
            (
                json!({"type":"rect","x":base,"y":0,"w":1,"h":1}),
                (1.0, 1.0),
            ),
            (
                json!({"type":"circle","cx":base,"cy":0,"r":0.25}),
                (0.5, 0.5),
            ),
            (
                json!({"type":"ellipse","cx":base,"cy":0,"rx":0.25,"ry":1}),
                (0.5, 2.0),
            ),
        ] {
            let vector: VectorData = serde_json::from_value(json!({
                "paths": [], "shapes": [shape]
            }))
            .unwrap();
            assert_eq!(intrinsic_size(&vector).unwrap(), expected);
        }
        let vector: VectorData = serde_json::from_value(json!({
            "paths": [],
            "shapes": [
                {"type":"rect","x":base,"y":0,"w":1,"h":1},
                {"type":"circle","cx":base+2.0,"cy":0,"r":0.25}
            ]
        }))
        .unwrap();
        assert_eq!(intrinsic_size(&vector).unwrap().0, 2.25);

        let vector: VectorData = serde_json::from_value(json!({
            "paths": [[
                {"type":"moveTo","x":base,"y":0},
                {"type":"curveTo","x1":base+2.0,"y1":0,
                    "x2":base+2.0,"y2":0,"x":base,"y":0}
            ]]
        }))
        .unwrap();
        assert_eq!(intrinsic_size(&vector).unwrap(), (1.5, 0.0));
    }

    #[test]
    fn subnormal_curve_extrema_do_not_collapse() {
        let tiny = f64::from_bits(1);
        let vector: VectorData = serde_json::from_value(json!({
            "paths": [[
                {"type":"moveTo","x":0,"y":0},
                {"type":"curveTo","x1":tiny,"y1":0,"x2":tiny,"y2":0,"x":0,"y":0}
            ]]
        }))
        .unwrap();
        assert_eq!(intrinsic_size(&vector).unwrap(), (tiny, 0.0));

        let mixed_scale: VectorData = serde_json::from_value(json!({
            "paths": [[
                {"type":"moveTo","x":0,"y":0},
                {"type":"curveTo","x1":tiny,"y1":0,"x2":1e308,"y2":0,"x":0,"y":0}
            ]]
        }))
        .unwrap();
        assert_eq!(
            intrinsic_size(&mixed_scale).unwrap_err().code,
            "INVALID_DOCUMENT"
        );
    }
}
