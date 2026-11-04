//! A light reader for geometry blobs: GeoPackage binary, PostGIS EWKB and
//! OGC/ISO WKB.
//!
//! This is a reader, not a geometry library. It decodes coordinates so a
//! plugin can count, measure or re-encode them, and offers a few cheap
//! checks ([`Geometry::bbox`], [`Geometry::num_points`],
//! [`Geometry::is_valid_basic`]). Topology (self-intersection, ring
//! orientation, …) is out of scope.

use std::fmt;

use serde_json::{json, Value};

/// Collections nested deeper than this are refused, so a hostile blob
/// cannot exhaust the stack.
const MAX_DEPTH: usize = 32;

/// The smallest possible WKB geometry (byte order, type, an empty count).
const MIN_WKB_BYTES: usize = 9;

/// One position. `z` and `m` are present when the geometry has them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coord {
    pub x: f64,
    pub y: f64,
    pub z: Option<f64>,
    pub m: Option<f64>,
}

impl Coord {
    /// A 2D position.
    pub fn xy(x: f64, y: f64) -> Self {
        Coord {
            x,
            y,
            z: None,
            m: None,
        }
    }

    /// x, y and (when present) z are finite. `m` is not checked: some
    /// systems use NaN for "no measure".
    fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_none_or(f64::is_finite)
    }
}

/// The seven OGC simple-feature geometry types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GeometryType {
    Point,
    LineString,
    Polygon,
    MultiPoint,
    MultiLineString,
    MultiPolygon,
    GeometryCollection,
}

impl GeometryType {
    /// The OGC name, e.g. `"MultiPolygon"` (also the GeoJSON `type`).
    pub fn as_str(self) -> &'static str {
        match self {
            GeometryType::Point => "Point",
            GeometryType::LineString => "LineString",
            GeometryType::Polygon => "Polygon",
            GeometryType::MultiPoint => "MultiPoint",
            GeometryType::MultiLineString => "MultiLineString",
            GeometryType::MultiPolygon => "MultiPolygon",
            GeometryType::GeometryCollection => "GeometryCollection",
        }
    }

    fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            1 => GeometryType::Point,
            2 => GeometryType::LineString,
            3 => GeometryType::Polygon,
            4 => GeometryType::MultiPoint,
            5 => GeometryType::MultiLineString,
            6 => GeometryType::MultiPolygon,
            7 => GeometryType::GeometryCollection,
            _ => return None,
        })
    }
}

impl fmt::Display for GeometryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How an extract stores geometry (its `geometry_format` metadata).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryFormat {
    /// GeoPackage binary: a `GP` header, then WKB.
    Gpkg,
    /// PostGIS extended WKB (plain and ISO WKB are read too).
    Ewkb,
}

impl GeometryFormat {
    /// Parse the metadata value (`"gpkg"` or `"ewkb"`, any case).
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "gpkg" => Some(GeometryFormat::Gpkg),
            "ewkb" | "wkb" => Some(GeometryFormat::Ewkb),
            _ => None,
        }
    }
}

/// Why a blob could not be decoded. Decoding never panics.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GeomError {
    #[error("geometry blob is truncated at byte {offset}")]
    Truncated { offset: usize },
    #[error("invalid WKB byte-order marker {0:#04x}")]
    ByteOrder(u8),
    #[error("unknown WKB geometry type {0:#x}")]
    UnknownType(u32),
    #[error("not GeoPackage binary (no \"GP\" magic)")]
    NotGeoPackage,
    #[error("unsupported GeoPackage binary version {0}")]
    GpkgVersion(u8),
    #[error("invalid GeoPackage envelope indicator {0}")]
    GpkgEnvelope(u8),
    #[error("extended GeoPackage geometry types are not supported")]
    GpkgExtended,
    #[error("a {container} may only hold {expected} members, found a {found}")]
    Member {
        container: GeometryType,
        expected: GeometryType,
        found: GeometryType,
    },
    #[error("geometry collections are nested more than {MAX_DEPTH} deep")]
    TooDeep,
    #[error("the geometry column holds {0}, not a BLOB")]
    NotBlob(&'static str),
}

/// A decoded geometry.
///
/// Empty geometries are represented naturally: `Point(None)` (WKB writes an
/// empty point as NaN coordinates), a `LineString` with no coordinates, a
/// `Polygon` with no rings, a collection with no members.
#[derive(Debug, Clone, PartialEq)]
pub enum Geometry {
    Point(Option<Coord>),
    LineString(Vec<Coord>),
    /// Rings; the first is the exterior.
    Polygon(Vec<Vec<Coord>>),
    MultiPoint(Vec<Option<Coord>>),
    MultiLineString(Vec<Vec<Coord>>),
    MultiPolygon(Vec<Vec<Vec<Coord>>>),
    GeometryCollection(Vec<Geometry>),
}

impl Geometry {
    /// Decode a blob, detecting the encoding: GeoPackage binary when it
    /// starts with `GP`, otherwise WKB / EWKB / ISO WKB.
    pub fn decode(bytes: &[u8]) -> Result<Geometry, GeomError> {
        if bytes.starts_with(b"GP") {
            Self::from_gpkg(bytes).map(|(g, _)| g)
        } else {
            Self::from_wkb(bytes).map(|(g, _)| g)
        }
    }

    /// Decode GeoPackage binary. Returns the geometry and the header's
    /// `srs_id`.
    pub fn from_gpkg(bytes: &[u8]) -> Result<(Geometry, i32), GeomError> {
        if bytes.len() < 2 || &bytes[..2] != b"GP" {
            return Err(if bytes.len() < 2 && b"GP".starts_with(bytes) {
                GeomError::Truncated {
                    offset: bytes.len(),
                }
            } else {
                GeomError::NotGeoPackage
            });
        }
        let mut r = Reader { buf: bytes, pos: 2 };
        let version = r.u8()?;
        if version != 0 {
            return Err(GeomError::GpkgVersion(version));
        }
        let flags = r.u8()?;
        if flags & 0b0010_0000 != 0 {
            return Err(GeomError::GpkgExtended);
        }
        let empty = flags & 0b0001_0000 != 0;
        let envelope = (flags >> 1) & 0b111;
        let envelope_len = match envelope {
            0 => 0,
            1 => 32,
            2 | 3 => 48,
            4 => 64,
            e => return Err(GeomError::GpkgEnvelope(e)),
        };
        let header_le = flags & 1 == 1;
        let srid = r.u32(header_le)? as i32;
        r.skip(envelope_len)?;
        if empty && r.remaining() == 0 {
            // Some writers flag an empty geometry and omit the WKB.
            return Ok((Geometry::GeometryCollection(Vec::new()), srid));
        }
        let (g, _) = read_geometry(&mut r, 0)?;
        Ok((g, srid))
    }

    /// Decode WKB: OGC WKB, ISO WKB (Z/M/ZM as type + 1000/2000/3000) and
    /// PostGIS EWKB (Z/M/SRID flag bits). Returns the geometry and the EWKB
    /// SRID when one is embedded. Bytes after the geometry are ignored.
    pub fn from_wkb(bytes: &[u8]) -> Result<(Geometry, Option<i32>), GeomError> {
        read_geometry(&mut Reader { buf: bytes, pos: 0 }, 0)
    }

    /// The geometry's type.
    pub fn geometry_type(&self) -> GeometryType {
        match self {
            Geometry::Point(_) => GeometryType::Point,
            Geometry::LineString(_) => GeometryType::LineString,
            Geometry::Polygon(_) => GeometryType::Polygon,
            Geometry::MultiPoint(_) => GeometryType::MultiPoint,
            Geometry::MultiLineString(_) => GeometryType::MultiLineString,
            Geometry::MultiPolygon(_) => GeometryType::MultiPolygon,
            Geometry::GeometryCollection(_) => GeometryType::GeometryCollection,
        }
    }

    /// Call `f` for every coordinate, in order.
    pub fn for_each_coord(&self, f: &mut dyn FnMut(&Coord)) {
        let line = |cs: &[Coord], f: &mut dyn FnMut(&Coord)| cs.iter().for_each(f);
        match self {
            Geometry::Point(p) => p.iter().for_each(f),
            Geometry::LineString(cs) => line(cs, f),
            Geometry::Polygon(rings) | Geometry::MultiLineString(rings) => {
                rings.iter().for_each(|r| line(r, f))
            }
            Geometry::MultiPoint(ps) => ps.iter().flatten().for_each(f),
            Geometry::MultiPolygon(polys) => polys.iter().flatten().for_each(|r| line(r, f)),
            Geometry::GeometryCollection(gs) => gs.iter().for_each(|g| g.for_each_coord(f)),
        }
    }

    /// The number of coordinates (an empty point has none).
    pub fn num_points(&self) -> usize {
        let mut n = 0;
        self.for_each_coord(&mut |_| n += 1);
        n
    }

    /// True when the geometry has no coordinates.
    pub fn is_empty(&self) -> bool {
        self.num_points() == 0
    }

    /// `[min_x, min_y, max_x, max_y]`, or `None` for an empty geometry.
    /// Coordinates with a NaN x or y are skipped.
    pub fn bbox(&self) -> Option<[f64; 4]> {
        let mut b = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        let mut any = false;
        self.for_each_coord(&mut |c| {
            if !c.x.is_nan() && !c.y.is_nan() {
                any = true;
                b[0] = b[0].min(c.x);
                b[1] = b[1].min(c.y);
                b[2] = b[2].max(c.x);
                b[3] = b[3].max(c.y);
            }
        });
        any.then_some(b)
    }

    /// Cheap structural checks: x, y (and z) are finite, every non-empty
    /// line string has at least 2 points, every polygon ring has at least 4
    /// points and is closed (first x,y equals last x,y). Empty geometries
    /// pass. Topology is not checked.
    pub fn is_valid_basic(&self) -> bool {
        fn line_ok(cs: &[Coord]) -> bool {
            cs.is_empty() || cs.len() >= 2
        }
        fn ring_ok(r: &[Coord]) -> bool {
            match (r.first(), r.last()) {
                (Some(a), Some(b)) => r.len() >= 4 && a.x == b.x && a.y == b.y,
                _ => false,
            }
        }
        fn polygon_ok(rings: &[Vec<Coord>]) -> bool {
            rings.iter().all(|r| ring_ok(r))
        }
        let shape_ok = match self {
            Geometry::Point(_) | Geometry::MultiPoint(_) => true,
            Geometry::LineString(cs) => line_ok(cs),
            Geometry::Polygon(rings) => polygon_ok(rings),
            Geometry::MultiLineString(lines) => lines.iter().all(|l| line_ok(l)),
            Geometry::MultiPolygon(polys) => polys.iter().all(|p| polygon_ok(p)),
            Geometry::GeometryCollection(gs) => gs.iter().all(Geometry::is_valid_basic),
        };
        let mut finite = true;
        self.for_each_coord(&mut |c| finite &= c.is_finite());
        shape_ok && finite
    }

    /// The geometry as a GeoJSON geometry object. Z is kept, M dropped,
    /// empty members of a MultiPoint are left out. Non-finite numbers
    /// become `null`.
    pub fn to_geojson(&self) -> Value {
        fn pos(c: &Coord) -> Value {
            match c.z {
                Some(z) => json!([c.x, c.y, z]),
                None => json!([c.x, c.y]),
            }
        }
        fn line(cs: &[Coord]) -> Value {
            Value::Array(cs.iter().map(pos).collect())
        }
        fn rings(rs: &[Vec<Coord>]) -> Value {
            Value::Array(rs.iter().map(|r| line(r)).collect())
        }
        let ty = self.geometry_type().as_str();
        let coordinates = match self {
            Geometry::GeometryCollection(gs) => {
                let members: Vec<Value> = gs.iter().map(Geometry::to_geojson).collect();
                return json!({"type": ty, "geometries": members});
            }
            Geometry::Point(p) => p.as_ref().map(pos).unwrap_or_else(|| json!([])),
            Geometry::LineString(cs) => line(cs),
            Geometry::Polygon(rs) | Geometry::MultiLineString(rs) => rings(rs),
            Geometry::MultiPoint(ps) => Value::Array(ps.iter().flatten().map(pos).collect()),
            Geometry::MultiPolygon(ps) => Value::Array(ps.iter().map(|p| rings(p)).collect()),
        };
        json!({"type": ty, "coordinates": coordinates})
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], GeomError> {
        if self.remaining() < N {
            return Err(GeomError::Truncated { offset: self.pos });
        }
        let mut out = [0u8; N];
        out.copy_from_slice(&self.buf[self.pos..self.pos + N]);
        self.pos += N;
        Ok(out)
    }

    fn skip(&mut self, n: usize) -> Result<(), GeomError> {
        if self.remaining() < n {
            return Err(GeomError::Truncated { offset: self.pos });
        }
        self.pos += n;
        Ok(())
    }

    fn u8(&mut self) -> Result<u8, GeomError> {
        Ok(self.take::<1>()?[0])
    }

    fn u32(&mut self, le: bool) -> Result<u32, GeomError> {
        let b = self.take::<4>()?;
        Ok(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn f64(&mut self, le: bool) -> Result<f64, GeomError> {
        let b = self.take::<8>()?;
        Ok(if le {
            f64::from_le_bytes(b)
        } else {
            f64::from_be_bytes(b)
        })
    }

    /// A count of items that each need at least `min_each` bytes. Checked
    /// against the bytes left, so a corrupt count cannot cause a huge
    /// allocation.
    fn count(&mut self, le: bool, min_each: usize) -> Result<usize, GeomError> {
        let at = self.pos;
        let n = self.u32(le)? as usize;
        match n.checked_mul(min_each) {
            Some(need) if need <= self.remaining() => Ok(n),
            _ => Err(GeomError::Truncated { offset: at }),
        }
    }
}

#[derive(Clone, Copy)]
struct Layout {
    le: bool,
    z: bool,
    m: bool,
}

impl Layout {
    fn coord_bytes(self) -> usize {
        8 * (2 + usize::from(self.z) + usize::from(self.m))
    }
}

fn read_coord(r: &mut Reader<'_>, l: Layout) -> Result<Coord, GeomError> {
    let x = r.f64(l.le)?;
    let y = r.f64(l.le)?;
    let z = if l.z { Some(r.f64(l.le)?) } else { None };
    let m = if l.m { Some(r.f64(l.le)?) } else { None };
    Ok(Coord { x, y, z, m })
}

fn read_point(r: &mut Reader<'_>, l: Layout) -> Result<Option<Coord>, GeomError> {
    let c = read_coord(r, l)?;
    Ok(if c.x.is_nan() && c.y.is_nan() {
        None
    } else {
        Some(c)
    })
}

fn read_line(r: &mut Reader<'_>, l: Layout) -> Result<Vec<Coord>, GeomError> {
    let n = r.count(l.le, l.coord_bytes())?;
    (0..n).map(|_| read_coord(r, l)).collect()
}

fn read_rings(r: &mut Reader<'_>, l: Layout) -> Result<Vec<Vec<Coord>>, GeomError> {
    let n = r.count(l.le, 4)?;
    (0..n).map(|_| read_line(r, l)).collect()
}

fn read_geometry(r: &mut Reader<'_>, depth: usize) -> Result<(Geometry, Option<i32>), GeomError> {
    if depth > MAX_DEPTH {
        return Err(GeomError::TooDeep);
    }
    let le = match r.u8()? {
        0 => false,
        1 => true,
        b => return Err(GeomError::ByteOrder(b)),
    };
    let raw = r.u32(le)?;
    // EWKB flag bits.
    let mut z = raw & 0x8000_0000 != 0;
    let mut m = raw & 0x4000_0000 != 0;
    let srid = if raw & 0x2000_0000 != 0 {
        Some(r.u32(le)? as i32)
    } else {
        None
    };
    // ISO WKB dimension offsets.
    let base = raw & 0x1FFF_FFFF;
    match base / 1000 {
        0 => {}
        1 => z = true,
        2 => m = true,
        3 => (z, m) = (true, true),
        _ => return Err(GeomError::UnknownType(raw)),
    }
    let ty = GeometryType::from_code(base % 1000).ok_or(GeomError::UnknownType(raw))?;
    let l = Layout { le, z, m };

    let members = |r: &mut Reader<'_>| r.count(le, MIN_WKB_BYTES);
    let wrong = |expected, found: &Geometry| GeomError::Member {
        container: ty,
        expected,
        found: found.geometry_type(),
    };
    let g = match ty {
        GeometryType::Point => Geometry::Point(read_point(r, l)?),
        GeometryType::LineString => Geometry::LineString(read_line(r, l)?),
        GeometryType::Polygon => Geometry::Polygon(read_rings(r, l)?),
        GeometryType::MultiPoint => {
            let n = members(r)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                match read_geometry(r, depth + 1)?.0 {
                    Geometry::Point(p) => out.push(p),
                    other => return Err(wrong(GeometryType::Point, &other)),
                }
            }
            Geometry::MultiPoint(out)
        }
        GeometryType::MultiLineString => {
            let n = members(r)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                match read_geometry(r, depth + 1)?.0 {
                    Geometry::LineString(cs) => out.push(cs),
                    other => return Err(wrong(GeometryType::LineString, &other)),
                }
            }
            Geometry::MultiLineString(out)
        }
        GeometryType::MultiPolygon => {
            let n = members(r)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                match read_geometry(r, depth + 1)?.0 {
                    Geometry::Polygon(rings) => out.push(rings),
                    other => return Err(wrong(GeometryType::Polygon, &other)),
                }
            }
            Geometry::MultiPolygon(out)
        }
        GeometryType::GeometryCollection => {
            let n = members(r)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push(read_geometry(r, depth + 1)?.0);
            }
            Geometry::GeometryCollection(out)
        }
    };
    Ok((g, srid))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny WKB writer for building test blobs.
    struct W {
        le: bool,
        out: Vec<u8>,
    }

    impl W {
        fn new(le: bool) -> Self {
            W {
                le,
                out: Vec::new(),
            }
        }
        fn header(&mut self, ty: u32) -> &mut Self {
            self.out.push(u8::from(self.le));
            self.u32(ty)
        }
        fn u32(&mut self, v: u32) -> &mut Self {
            let b = if self.le {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            };
            self.out.extend_from_slice(&b);
            self
        }
        fn f(&mut self, vs: &[f64]) -> &mut Self {
            for v in vs {
                let b = if self.le {
                    v.to_le_bytes()
                } else {
                    v.to_be_bytes()
                };
                self.out.extend_from_slice(&b);
            }
            self
        }
        fn line(&mut self, pts: &[[f64; 2]]) -> &mut Self {
            self.u32(pts.len() as u32);
            for p in pts {
                self.f(p);
            }
            self
        }
        fn bytes(&self) -> Vec<u8> {
            self.out.clone()
        }
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn gpkg(flags: u8, srid: i32, envelope: &[f64], wkb: &[u8]) -> Vec<u8> {
        let le = flags & 1 == 1;
        let mut b = vec![b'G', b'P', 0, flags];
        b.extend_from_slice(&if le {
            srid.to_le_bytes()
        } else {
            srid.to_be_bytes()
        });
        for v in envelope {
            b.extend_from_slice(&if le { v.to_le_bytes() } else { v.to_be_bytes() });
        }
        b.extend_from_slice(wkb);
        b
    }

    const SQUARE: [[f64; 2]; 5] = [[0.0, 0.0], [4.0, 0.0], [4.0, 3.0], [0.0, 3.0], [0.0, 0.0]];

    #[test]
    fn known_point_encodings() {
        // POINT(1 2) little and big endian (independent reference bytes).
        let le = hex("0101000000000000000000F03F0000000000000040");
        let be = hex("00000000013FF00000000000004000000000000000");
        for b in [le, be] {
            let (g, srid) = Geometry::from_wkb(&b).unwrap();
            assert_eq!(g, Geometry::Point(Some(Coord::xy(1.0, 2.0))));
            assert_eq!(srid, None);
        }
        // EWKB SRID=4326;POINT(1 2), as PostGIS writes it.
        let ewkb = hex("0101000020E6100000000000000000F03F0000000000000040");
        let (g, srid) = Geometry::from_wkb(&ewkb).unwrap();
        assert_eq!(g.bbox(), Some([1.0, 2.0, 1.0, 2.0]));
        assert_eq!(srid, Some(4326));
    }

    #[test]
    fn every_type_both_byte_orders() {
        for le in [true, false] {
            let point = W::new(le).header(1).f(&[1.0, 2.0]).bytes();
            let line = W::new(le).header(2).line(&[[0.0, 0.0], [3.0, 4.0]]).bytes();
            let poly = {
                let mut w = W::new(le);
                w.header(3).u32(1).line(&SQUARE);
                w.bytes()
            };
            let multi = |ty: u32, parts: &[&Vec<u8>]| {
                let mut w = W::new(le);
                w.header(ty).u32(parts.len() as u32);
                for p in parts {
                    w.out.extend_from_slice(p);
                }
                w.bytes()
            };

            let g = Geometry::decode(&point).unwrap();
            assert_eq!(g.geometry_type(), GeometryType::Point);
            assert_eq!(g.bbox(), Some([1.0, 2.0, 1.0, 2.0]));

            let g = Geometry::decode(&line).unwrap();
            assert_eq!(g.geometry_type(), GeometryType::LineString);
            assert_eq!(g.num_points(), 2);
            assert_eq!(g.bbox(), Some([0.0, 0.0, 3.0, 4.0]));
            assert!(g.is_valid_basic());

            let g = Geometry::decode(&poly).unwrap();
            assert_eq!(g.geometry_type(), GeometryType::Polygon);
            assert_eq!(g.num_points(), 5);
            assert!(g.is_valid_basic());

            let g = Geometry::decode(&multi(4, &[&point, &point])).unwrap();
            assert_eq!(g, Geometry::MultiPoint(vec![Some(Coord::xy(1.0, 2.0)); 2]));

            let g = Geometry::decode(&multi(5, &[&line, &line])).unwrap();
            assert_eq!(g.geometry_type(), GeometryType::MultiLineString);
            assert_eq!(g.num_points(), 4);

            let g = Geometry::decode(&multi(6, &[&poly])).unwrap();
            assert_eq!(g.geometry_type(), GeometryType::MultiPolygon);
            assert_eq!(g.bbox(), Some([0.0, 0.0, 4.0, 3.0]));

            let g = Geometry::decode(&multi(7, &[&point, &line, &poly])).unwrap();
            assert_eq!(g.geometry_type(), GeometryType::GeometryCollection);
            assert_eq!(g.num_points(), 8);
            assert_eq!(g.bbox(), Some([0.0, 0.0, 4.0, 4.0]));
            assert!(g.is_valid_basic());
        }
    }

    #[test]
    fn mixed_byte_orders_inside_a_collection() {
        let inner_be = W::new(false).header(1).f(&[5.0, 6.0]).bytes();
        let mut w = W::new(true);
        w.header(4).u32(1);
        w.out.extend_from_slice(&inner_be);
        let g = Geometry::decode(&w.bytes()).unwrap();
        assert_eq!(g, Geometry::MultiPoint(vec![Some(Coord::xy(5.0, 6.0))]));
    }

    #[test]
    fn z_and_m_dimensions() {
        // ISO WKB: Point Z = 1001, LineString M = 2002, Point ZM = 3001.
        let pz = W::new(true).header(1001).f(&[1.0, 2.0, 3.0]).bytes();
        let g = Geometry::decode(&pz).unwrap();
        assert_eq!(
            g,
            Geometry::Point(Some(Coord {
                x: 1.0,
                y: 2.0,
                z: Some(3.0),
                m: None
            }))
        );
        assert_eq!(g.to_geojson()["coordinates"], json!([1.0, 2.0, 3.0]));

        let lm = W::new(false)
            .header(2002)
            .u32(2)
            .f(&[0.0, 0.0, 10.0, 1.0, 1.0, 20.0])
            .bytes();
        match Geometry::decode(&lm).unwrap() {
            Geometry::LineString(cs) => {
                assert_eq!(
                    cs[1],
                    Coord {
                        x: 1.0,
                        y: 1.0,
                        z: None,
                        m: Some(20.0)
                    }
                )
            }
            g => panic!("{g:?}"),
        }

        let pzm = W::new(true).header(3001).f(&[1.0, 2.0, 3.0, 4.0]).bytes();
        let g = Geometry::decode(&pzm).unwrap();
        assert_eq!(
            g,
            Geometry::Point(Some(Coord {
                x: 1.0,
                y: 2.0,
                z: Some(3.0),
                m: Some(4.0)
            }))
        );

        // EWKB: Z flag + SRID flag on a point; Z|M flags on a line.
        let ewkb = W::new(true)
            .header(0xA000_0001)
            .u32(2277)
            .f(&[1.0, 2.0, 3.0])
            .bytes();
        let (g, srid) = Geometry::from_wkb(&ewkb).unwrap();
        assert_eq!(srid, Some(2277));
        assert_eq!(
            g,
            Geometry::Point(Some(Coord {
                x: 1.0,
                y: 2.0,
                z: Some(3.0),
                m: None
            }))
        );
        let ewkb_zm = W::new(false)
            .header(0xC000_0002)
            .u32(1)
            .f(&[1.0, 2.0, 3.0, 4.0])
            .bytes();
        assert_eq!(Geometry::decode(&ewkb_zm).unwrap().num_points(), 1);
        // PostGIS reference: POINT Z (1 2 3) without SRID.
        let pg = hex("0101000080000000000000F03F00000000000000400000000000000840");
        assert_eq!(
            Geometry::decode(&pg).unwrap().to_geojson()["coordinates"],
            json!([1.0, 2.0, 3.0])
        );
    }

    #[test]
    fn empty_geometries() {
        // POINT EMPTY is written as NaN coordinates.
        let empty_point = W::new(true).header(1).f(&[f64::NAN, f64::NAN]).bytes();
        let g = Geometry::decode(&empty_point).unwrap();
        assert_eq!(g, Geometry::Point(None));
        assert!(g.is_empty());
        assert_eq!(g.bbox(), None);
        assert_eq!(g.num_points(), 0);
        assert!(g.is_valid_basic());
        assert_eq!(g.to_geojson(), json!({"type": "Point", "coordinates": []}));

        let empty_line = W::new(false).header(2).u32(0).bytes();
        let g = Geometry::decode(&empty_line).unwrap();
        assert!(g.is_empty() && g.is_valid_basic() && g.bbox().is_none());

        let empty_gc = W::new(true).header(7).u32(0).bytes();
        assert_eq!(
            Geometry::decode(&empty_gc).unwrap(),
            Geometry::GeometryCollection(vec![])
        );

        // A multipoint with one empty member keeps the slot; bbox skips it.
        let mut w = W::new(true);
        w.header(4).u32(2);
        w.out
            .extend(W::new(true).header(1).f(&[f64::NAN, f64::NAN]).bytes());
        w.out.extend(W::new(true).header(1).f(&[7.0, 8.0]).bytes());
        let g = Geometry::decode(&w.bytes()).unwrap();
        assert_eq!(
            g,
            Geometry::MultiPoint(vec![None, Some(Coord::xy(7.0, 8.0))])
        );
        assert_eq!(g.bbox(), Some([7.0, 8.0, 7.0, 8.0]));
    }

    #[test]
    fn basic_validity() {
        let unclosed = W::new(true).header(3).u32(1).line(&SQUARE[..4]).bytes();
        assert!(!Geometry::decode(&unclosed).unwrap().is_valid_basic());

        let mut open5 = SQUARE;
        open5[4] = [0.5, 0.0];
        let g = Geometry::decode(&W::new(true).header(3).u32(1).line(&open5).bytes()).unwrap();
        assert!(!g.is_valid_basic());

        let short_ring = W::new(true)
            .header(3)
            .u32(1)
            .line(&[[0.0, 0.0], [1.0, 1.0], [0.0, 0.0]])
            .bytes();
        assert!(!Geometry::decode(&short_ring).unwrap().is_valid_basic());

        let one_point_line = W::new(true).header(2).line(&[[1.0, 1.0]]).bytes();
        assert!(!Geometry::decode(&one_point_line).unwrap().is_valid_basic());

        let infinite = W::new(true).header(1).f(&[f64::INFINITY, 0.0]).bytes();
        assert!(!Geometry::decode(&infinite).unwrap().is_valid_basic());

        let half_nan = W::new(true)
            .header(2)
            .line(&[[0.0, 0.0], [f64::NAN, 1.0]])
            .bytes();
        assert!(!Geometry::decode(&half_nan).unwrap().is_valid_basic());

        // An invalid member makes the collection invalid.
        let mut w = W::new(true);
        w.header(7).u32(1);
        w.out.extend(&one_point_line);
        assert!(!Geometry::decode(&w.bytes()).unwrap().is_valid_basic());
    }

    #[test]
    fn geopackage_headers_and_envelopes() {
        let wkb = W::new(true)
            .header(2)
            .line(&[[0.0, 0.0], [3.0, 4.0]])
            .bytes();
        // Envelope indicator 0..=4 → 0, 32, 48, 48, 64 bytes; header byte order both ways.
        for (indicator, doubles) in [(0u8, 0usize), (1, 4), (2, 6), (3, 6), (4, 8)] {
            for header_le in [0u8, 1] {
                let flags = (indicator << 1) | header_le;
                let env = vec![9.0; doubles];
                let blob = gpkg(flags, 3081, &env, &wkb);
                let (g, srid) = Geometry::from_gpkg(&blob).unwrap();
                assert_eq!(srid, 3081, "indicator {indicator}, le {header_le}");
                assert_eq!(g.bbox(), Some([0.0, 0.0, 3.0, 4.0]));
                assert_eq!(Geometry::decode(&blob).unwrap(), g);
            }
        }
        // Header little endian, WKB big endian.
        let be = W::new(false).header(1).f(&[1.0, 2.0]).bytes();
        let g = Geometry::decode(&gpkg(0b0000_0011, 4326, &[1.0, 1.0, 2.0, 2.0], &be)).unwrap();
        assert_eq!(g, Geometry::Point(Some(Coord::xy(1.0, 2.0))));

        // Empty flag with an empty point, and with no WKB at all.
        let nan = W::new(true).header(1).f(&[f64::NAN, f64::NAN]).bytes();
        assert_eq!(
            Geometry::decode(&gpkg(0b0001_0001, 4326, &[], &nan)).unwrap(),
            Geometry::Point(None)
        );
        assert!(Geometry::decode(&gpkg(0b0001_0001, 4326, &[], &[]))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn geopackage_errors() {
        let wkb = W::new(true).header(1).f(&[1.0, 2.0]).bytes();
        let mut v1 = gpkg(1, 0, &[], &wkb);
        v1[2] = 1;
        assert_eq!(Geometry::from_gpkg(&v1), Err(GeomError::GpkgVersion(1)));
        assert_eq!(
            Geometry::from_gpkg(&gpkg(5 << 1 | 1, 0, &[], &wkb)),
            Err(GeomError::GpkgEnvelope(5))
        );
        assert_eq!(
            Geometry::from_gpkg(&gpkg(0b0010_0001, 0, &[], &wkb)),
            Err(GeomError::GpkgExtended)
        );
        assert_eq!(Geometry::from_gpkg(&wkb), Err(GeomError::NotGeoPackage));
        // Envelope promised but missing.
        assert!(matches!(
            Geometry::from_gpkg(&gpkg(4 << 1 | 1, 0, &[1.0], &[])),
            Err(GeomError::Truncated { .. })
        ));
    }

    #[test]
    fn truncated_and_corrupt_input_is_an_error() {
        let mut poly = W::new(true);
        poly.header(3).u32(1).line(&SQUARE);
        let full = poly.bytes();
        // Every proper prefix fails cleanly.
        for n in 0..full.len() {
            assert!(Geometry::decode(&full[..n]).is_err(), "prefix {n}");
        }
        let gp = gpkg(0b0000_0011, 4326, &[0.0, 4.0, 0.0, 3.0], &full);
        for n in 0..gp.len() {
            assert!(Geometry::decode(&gp[..n]).is_err(), "gpkg prefix {n}");
        }
        assert!(matches!(
            Geometry::decode(&[]),
            Err(GeomError::Truncated { offset: 0 })
        ));
        assert_eq!(
            Geometry::decode(&[2, 1, 0, 0, 0]),
            Err(GeomError::ByteOrder(2))
        );
        assert!(matches!(
            Geometry::decode(&W::new(true).header(8).bytes()),
            Err(GeomError::UnknownType(8))
        ));
        assert!(matches!(
            Geometry::decode(&W::new(true).header(4001).bytes()),
            Err(GeomError::UnknownType(_))
        ));
        // A huge count with no data behind it is refused before allocating.
        let huge = W::new(true).header(2).u32(u32::MAX).bytes();
        assert!(matches!(
            Geometry::decode(&huge),
            Err(GeomError::Truncated { .. })
        ));
        // A MultiPoint holding a LineString.
        let mut w = W::new(true);
        w.header(4).u32(1);
        w.out.extend(
            W::new(true)
                .header(2)
                .line(&[[0.0, 0.0], [1.0, 1.0]])
                .bytes(),
        );
        assert!(matches!(
            Geometry::decode(&w.bytes()),
            Err(GeomError::Member { .. })
        ));
        // Deeply nested collections.
        let mut deep = Vec::new();
        for _ in 0..40 {
            deep.extend(W::new(true).header(7).u32(1).bytes());
        }
        deep.extend(W::new(true).header(7).u32(0).bytes());
        assert_eq!(Geometry::decode(&deep), Err(GeomError::TooDeep));
    }

    #[test]
    fn geojson_output() {
        let mut w = W::new(true);
        w.header(6).u32(1);
        w.out
            .extend(W::new(true).header(3).u32(1).line(&SQUARE).bytes());
        let g = Geometry::decode(&w.bytes()).unwrap();
        let j = g.to_geojson();
        assert_eq!(j["type"], "MultiPolygon");
        assert_eq!(j["coordinates"][0][0][2], json!([4.0, 3.0]));
        let gc = Geometry::GeometryCollection(vec![g]);
        assert_eq!(gc.to_geojson()["geometries"][0]["type"], "MultiPolygon");
    }

    #[test]
    fn format_names() {
        assert_eq!(
            GeometryFormat::from_name("GPKG"),
            Some(GeometryFormat::Gpkg)
        );
        assert_eq!(
            GeometryFormat::from_name("ewkb"),
            Some(GeometryFormat::Ewkb)
        );
        assert_eq!(GeometryFormat::from_name(""), None);
    }
}
