---
name: maps-and-deliverables
description: Making maps and data deliverables that meet a spec, covering layout elements (scale bar, north arrow where meaningful, legend, data sources and dates, CRS note), symbology and classification methods (natural breaks, quantile, equal interval), colour-blind safe palettes, ISO 19115 and FGDC metadata basics, and checking a delivery spec for CRS, formats, field renames and required metadata. Use it when asked to "make a map", "export a PDF", "symbolize this layer", "pick colours", "write metadata", "package the deliverable", "send this to the district", or before handing any map or data to someone else.
---

# Maps and deliverables

A deliverable is finished when the person receiving it can use it without asking you
anything: right CRS, right format, right field names, sources and dates stated, and a map a
reader can interpret correctly. Get the spec first, then build to it, then check against it.

## General practice (with or without Columbus)

### 1. Get the spec before you start

Ask or look up, and write down:

- **CRS** (horizontal and vertical, with EPSG codes and units) and whether it is a hard rule.
- **Formats**: GeoPackage, file geodatabase, shapefile, CSV, KML, PDF, PNG; one file or a
  zipped package; naming convention for files and layers.
- **Fields**: which to include, renames, order, types, whether coded fields go as codes,
  labels or both, date format.
- **Metadata** required (title, abstract, sources, dates, contact, constraints) and in what
  standard (ISO 19115/19139, FGDC CSDGM, or the recipient's own form).
- **Map products**: page size, orientation, resolution, logo and disclaimer text.
- Who receives it, by when, and how (shared drive, portal, email size limit).

### 2. Layout elements

| Element | Include when | Notes |
| --- | --- | --- |
| Title | Always | Says what, where and when ("Functional class, District 14 roads, October 2026") |
| Legend | Whenever symbols need explaining | Only layers shown; labels in plain words, with units; no "Layer1" or field names |
| Scale bar | Large and medium scales in a projected CRS | Units the reader uses (feet and miles for a US county). Leave it off small-scale maps where scale varies across the page (world, continental) |
| North arrow | When north is not obviously up, or the reader expects one | Omit it, or use a graticule, where north changes across the map (polar, many conic small-scale maps). Never point it wrong on a rotated data frame |
| Data sources and dates | Always | Each dataset, its publisher and its date or vintage |
| CRS note | Always for maps that will be measured or compared | "NAD83 / Texas Centric Lambert Conformal (EPSG:3081)" |
| Author, organization, date produced | Always | |
| Disclaimer | When the organization requires one | Use its exact wording |
| Inset or locator | When readers may not know where this is | |

Visual hierarchy: the subject layer strongest; reference layers (roads, boundaries,
hillshade) muted grey. Labels must not collide or hide the data.

Exporting: vector PDF for print (fonts embedded), 300 dpi for raster content in print, 150
dpi for screen. In ArcGIS Pro:

```python
aprx = arcpy.mp.ArcGISProject("CURRENT")
layout = aprx.listLayouts("District Map")[0]
layout.exportToPDF(r"C:\deliver\d14_functional_class_2026-11.pdf", resolution=300,
                   image_quality="BEST", embed_fonts=True)
```

In QGIS, the print layout's Export as PDF with "Always export as vectors" on.

### 3. Symbology and classification

| Method | Use when | Watch out |
| --- | --- | --- |
| Natural breaks (Jenks) | Data with clusters; one-off maps | Breaks change with the data, so maps over time are not comparable |
| Quantile | Ranking; equal counts per class | Similar values can land in different classes; skewed data looks evenly spread |
| Equal interval | Familiar ranges (percent, temperature); comparisons across maps | Most features may fall in one or two classes on skewed data |
| Standard deviation | Departure from the mean | Readers need the mean stated |
| Manual (defined) | Meaningful thresholds (regulatory limits, speed classes) | State why the breaks are there |
| Unique values | Categories (land use, functional class) | At most about 8 to 10 colours; group the rest |

- **Normalise choropleths**: map rates or densities (per person, per square mile, percent),
  not raw counts. Show counts with graduated or proportional symbols.
- 4 to 7 classes for choropleths. Show the class breaks in the legend with sensible rounding.
- Say how classes were made in a note when it matters to interpretation.

### 4. Colour

- **Sequential** (light to dark, one hue or two) for low-to-high magnitudes.
- **Diverging** (two hues meeting at a neutral midpoint) only when the midpoint means
  something: zero change, the state average.
- **Qualitative** (distinct hues of similar lightness) for categories.
- **Colour-blind safe**: about 1 in 12 men has a colour vision deficiency, most often
  red-green. Avoid red-versus-green contrasts. Good sources:
  - ColorBrewer schemes marked colour-blind safe (YlGnBu, Blues, PuOr, RdBu, BrBG; Set2 or
    Dark2 for up to 6 to 8 categories).
  - viridis and cividis for continuous rasters.
  - Okabe-Ito qualitative: `#E69F00` orange, `#56B4E9` sky blue, `#009E73` bluish green,
    `#F0E442` yellow, `#0072B2` blue, `#D55E00` vermillion, `#CC79A7` reddish purple, `#000000` black.
- Do not use rainbow ramps for continuous data; they create false boundaries.
- Do not rely on colour alone: add labels, patterns or line styles for key distinctions, and
  check the map with a colour-blindness simulator and in greyscale.

### 5. Metadata basics

Minimum for any delivered dataset, whatever the standard:

1. Title and abstract (what it is, in two or three sentences).
2. Purpose and intended use; limitations.
3. Dates: when the data describes (temporal extent), when produced, update frequency.
4. Spatial extent (bounding box) and CRS (EPSG codes, horizontal and vertical).
5. Lineage: source datasets with their dates, and the processing steps.
6. Attributes: each field's meaning, units and code lists.
7. Contact (organization, role, email), distribution and use constraints or licence.

**ISO 19115** (19115-1 for vector, 19115-2 adds imagery and gridded data; XML encoded as
ISO 19139 or 19115-3) and **FGDC CSDGM** (US federal; sections 1 Identification, 2 Data
Quality, 3 Spatial Data Organization, 4 Spatial Reference, 5 Entity and Attribute, 6
Distribution, 7 Metadata Reference) cover the same ground. Element names and a checklist:
[references/metadata-and-formats.md](references/metadata-and-formats.md).

In ArcGIS Pro:

```python
from arcpy import metadata as md
m = md.Metadata(r"C:\deliver\d14.gdb\roadway_inventory")   # a dataset Pro manages metadata for
m.title = "Roadway inventory, District 14"
m.summary = "Active county roadway segments with functional class."
m.description = "Source: ROADS.ROADWAY_INVENTORY, DEFAULT as of 2026-11-04. Retired segments removed."
m.tags = "roads, functional class, District 14"
m.credits = "Contoso County GIS"
m.accessConstraints = "Internal use only."
m.save()
m.exportMetadata(r"C:\deliver\d14_roadway_inventory.xml", "ISO19139")   # or "FGDC_CSDGM"
```

### 6. Meeting the delivery spec

Work in this order, on a copy:

1. **Filter** to what was asked (for example active segments only).
2. **Reproject** to the delivery CRS. Check datum realisation as well as code (NAD83(2011)
   is not NAD83(HARN)), and units.
3. **Rename and order fields** to the spec; drop fields not asked for, especially internal or
   sensitive ones.
4. **Codes and labels** as specified (add a label field if asked; never overwrite the code).
5. **Write the format.** Know its limits:

| Format | Limits that bite |
| --- | --- |
| Shapefile | Field names 10 characters, 2 GB per file, one geometry type, no true NULL in numbers, dates without times, `.prj` may lack a vertical CRS, needs `.shp/.shx/.dbf/.prj` (and `.cpg` for UTF-8) together |
| GeoPackage | Good default; one file, many layers; check layer names match the spec |
| File geodatabase | Folder that must be zipped whole; Esri-centric |
| CSV | No geometry unless WKT or X/Y columns; state the CRS of the coordinates; codes can lose zero padding in Excel |
| KML/KMZ | Always WGS 84 lon/lat; styling only |

6. **Check against the spec**, item by item, by reopening the written files (not your
   in-memory layers): CRS reported by the file, field names after writing (shapefile
   truncation), row counts against the source with the filter, geometry valid, no empty
   geometries, metadata present, file names as specified.
7. **Package** with a short readme: contents, CRS, sources and dates, contact.

## If the Columbus tools are available

Columbus enforces its rules in the server; this only explains them.

**Read the organization's delivery spec from the context pack:**

```json
context_search {"query": "district deliverable", "kind": "delivery"}
context_get {"id": "district-deliverable"}
```

A delivery entry carries `crs`, `formats`, field renames (for example
`fields: {rename: {FUNC_CLASS: FUNCTIONAL_CLASS}}`) and required `metadata` items. `whoami`
also returns the pack's always-on rules ("deliverables in EPSG:3081"). An entry marked
`stale` is past its review date: say so and ask whether it still applies. Columbus does not
yet enforce delivery specs itself, so checking against the entry is your job.

**Prepare and check the layers:**

- `query_features` for the filtered rows, then
  `vector_op {"op": "reproject", "inputs": {"layer": "<handle>"}, "params": {"crs": "EPSG:3081"}}`
  to the delivery CRS.
- **Run `verify` on every layer before you hand it over**:
  `verify {"layer": "ws://<session>/results/<file>.gpkg"}`. Report the checks
  (`geometry.valid`, `geometry.empty`, `crs.*`, `query.read_limit`) to the person. Do not
  deliver a layer marked `input.truncated` or cut off at the read limit as if complete.
- `list_outputs` lists the session's files with row counts, data labels and provenance;
  include the provenance (sources, filters, steps) in the readme and metadata lineage.

**What is not available yet:**

- **Exporting to the delivery format and folder** (`export_output`, which will apply the
  delivery entry's CRS, formats, renames and metadata) is planned and not yet available.
  Columbus's results stay in the session workspace as GeoPackage (or CSV) handles. Tell the
  person which handles are final, and give them the field renames and metadata text to apply
  when they export in Pro or QGIS.
- Columbus has no tool to rename fields or write metadata files today.
- Any upload of governed data to a connected server (Drive, a portal) needs the person's
  approval while its data labels are active, and organization flow rules can forbid it. The
  approval goes to the person, never to you.
