// Geodesic distance on the WGS 84 ellipsoid, for the map's measure tool.
//
// Vincenty's inverse formula (Survey Review, 1975): accurate to well under a
// millimetre for any two points that are not nearly antipodal. For those,
// where the iteration does not settle, the great-circle distance on a sphere
// of the ellipsoid's mean radius is used (within 0.5%).

const A = 6378137.0;
const F = 1 / 298.257223563;
const B = A * (1 - F);
const R_MEAN = (2 * A + B) / 3;
const RAD = Math.PI / 180;

function sphere(lon1: number, lat1: number, lon2: number, lat2: number): number {
  const p1 = lat1 * RAD;
  const p2 = lat2 * RAD;
  const dp = p2 - p1;
  const dl = (lon2 - lon1) * RAD;
  const h = Math.sin(dp / 2) ** 2 + Math.cos(p1) * Math.cos(p2) * Math.sin(dl / 2) ** 2;
  return 2 * R_MEAN * Math.asin(Math.min(1, Math.sqrt(h)));
}

/** Metres between two points given as [longitude, latitude] in degrees. */
export function geodesicMetres(a: [number, number], b: [number, number]): number {
  const [lon1, lat1] = a;
  const [lon2, lat2] = b;
  if (lon1 === lon2 && lat1 === lat2) return 0;
  const L = (lon2 - lon1) * RAD;
  const U1 = Math.atan((1 - F) * Math.tan(lat1 * RAD));
  const U2 = Math.atan((1 - F) * Math.tan(lat2 * RAD));
  const sinU1 = Math.sin(U1);
  const cosU1 = Math.cos(U1);
  const sinU2 = Math.sin(U2);
  const cosU2 = Math.cos(U2);
  let lambda = L;
  for (let i = 0; i < 200; i++) {
    const sinL = Math.sin(lambda);
    const cosL = Math.cos(lambda);
    const sinSigma = Math.sqrt((cosU2 * sinL) ** 2 + (cosU1 * sinU2 - sinU1 * cosU2 * cosL) ** 2);
    if (sinSigma === 0) return 0;
    const cosSigma = sinU1 * sinU2 + cosU1 * cosU2 * cosL;
    const sigma = Math.atan2(sinSigma, cosSigma);
    const sinAlpha = (cosU1 * cosU2 * sinL) / sinSigma;
    const cos2Alpha = 1 - sinAlpha * sinAlpha;
    const cos2SigmaM = cos2Alpha !== 0 ? cosSigma - (2 * sinU1 * sinU2) / cos2Alpha : 0;
    const C = (F / 16) * cos2Alpha * (4 + F * (4 - 3 * cos2Alpha));
    const prev = lambda;
    lambda =
      L +
      (1 - C) * F * sinAlpha * (sigma + C * sinSigma * (cos2SigmaM + C * cosSigma * (-1 + 2 * cos2SigmaM ** 2)));
    if (Math.abs(lambda - prev) < 1e-12) {
      const u2 = (cos2Alpha * (A * A - B * B)) / (B * B);
      const AA = 1 + (u2 / 16384) * (4096 + u2 * (-768 + u2 * (320 - 175 * u2)));
      const BB = (u2 / 1024) * (256 + u2 * (-128 + u2 * (74 - 47 * u2)));
      const dSigma =
        BB *
        sinSigma *
        (cos2SigmaM +
          (BB / 4) *
            (cosSigma * (-1 + 2 * cos2SigmaM ** 2) -
              (BB / 6) * cos2SigmaM * (-3 + 4 * sinSigma ** 2) * (-3 + 4 * cos2SigmaM ** 2)));
      return B * AA * (sigma - dSigma);
    }
  }
  return sphere(lon1, lat1, lon2, lat2);
}

/** Metres along a line of [longitude, latitude] points. */
export function lineMetres(points: [number, number][]): number {
  let total = 0;
  for (let i = 1; i < points.length; i++) total += geodesicMetres(points[i - 1], points[i]);
  return total;
}

const FT_PER_M = 1 / 0.3048;

/** A length in feet first (the analyst's unit), then metres. */
export function formatLength(m: number): { feet: string; metres: string } {
  const ft = m * FT_PER_M;
  const feet = `${ft.toLocaleString("en-US", { maximumFractionDigits: ft < 100 ? 1 : 0 })} ft`;
  const metres = `${m.toLocaleString("en-US", { maximumFractionDigits: m < 1000 ? 1 : 0 })} m`;
  return { feet, metres };
}
