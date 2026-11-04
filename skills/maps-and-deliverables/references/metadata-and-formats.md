# Metadata elements and delivery checklist

## The same idea in ISO 19115 and FGDC CSDGM

| What | ISO 19115 element (19139 XML path, simplified) | FGDC CSDGM element |
| --- | --- | --- |
| Title | `MD_DataIdentification/citation/title` | 1.1 Citation > Title |
| Abstract | `MD_DataIdentification/abstract` | 1.2 Description > Abstract |
| Purpose | `MD_DataIdentification/purpose` | 1.2 Description > Purpose |
| Dates (creation, publication, revision) | `citation/date` with `dateType` | 1.1 Citation > Publication Date |
| Temporal extent | `extent/temporalElement` | 1.3 Time Period of Content |
| Update frequency | `resourceMaintenance/maintenanceAndUpdateFrequency` | 1.4 Status > Maintenance and Update Frequency |
| Bounding box | `extent/geographicElement/EX_GeographicBoundingBox` | 1.5 Spatial Domain > Bounding Coordinates |
| Keywords | `descriptiveKeywords` | 1.6 Keywords (theme, place) |
| Use constraints, licence | `resourceConstraints` (`MD_LegalConstraints`) | 1.7 Access Constraints, 1.8 Use Constraints |
| Contact | `pointOfContact` (`CI_ResponsibleParty`) | 1.9 Point of Contact |
| Lineage: sources | `dataQualityInfo/lineage/source` | 2.5.1 Source Information |
| Lineage: processing steps | `dataQualityInfo/lineage/processStep` | 2.5.2 Process Step |
| Positional accuracy | `dataQualityInfo/report` (`DQ_AbsoluteExternalPositionalAccuracy`) | 2.4 Positional Accuracy |
| Completeness | `DQ_CompletenessOmission`, `DQ_CompletenessCommission` | 2.3 Completeness Report |
| CRS | `referenceSystemInfo` (EPSG code) | 4 Spatial Reference Information |
| Feature and attribute definitions | `contentInfo` (`MD_FeatureCatalogueDescription`; ISO 19110 catalogue) | 5 Entity and Attribute Information |
| Distribution format | `distributionInfo/distributionFormat` | 6 Distribution Information |
| Metadata date and contact | `dateStamp`, `contact` | 7 Metadata Reference Information |

ArcGIS Pro metadata styles (Options > Metadata): Item Description, FGDC CSDGM Metadata,
INSPIRE Metadata Directive, ISO 19139 Metadata Implementation Specification, North American
Profile of ISO 19115 2003. Pick the style the recipient asked for so Pro shows the right
pages, then export with `Metadata.exportMetadata(path, "ISO19139")` or `"FGDC_CSDGM"`.

## Lineage text that works

> Source: ROADS.ROADWAY_INVENTORY (Contoso County GIS, enterprise geodatabase, DEFAULT
> version as of 2026-11-04 08:00). Filtered to STATUS = 'A'. Reprojected from EPSG:2277
> to EPSG:3081. Field FUNC_CLASS renamed FUNCTIONAL_CLASS; labels from domain
> functional-class (reviewed 2026-10-15). Geometry validated; 0 invalid, 0 empty.

## Delivery checklist

Copy into the readme or the hand-over note and tick each item against the spec.

- [ ] CRS of every layer matches the spec (read back from the written file)
- [ ] Vertical datum and units stated, if heights are included
- [ ] Format and file names as specified; layer names as specified
- [ ] Fields: included set, renames, order, types; no internal or hidden fields
- [ ] Coded fields as codes, labels or both, as specified
- [ ] Row count equals the source count under the stated filter
- [ ] No invalid or empty geometry; one geometry type per layer
- [ ] Dates in the specified format and time zone
- [ ] Metadata present with every required item
- [ ] Map PDF: title, legend, scale bar and north arrow where meaningful, sources and
      dates, CRS note, author, date, disclaimer
- [ ] Readme: contents, CRS, sources and dates, processing summary, contact
- [ ] Package opened fresh on another machine or folder, and it works

## Field name rules by format

| Format | Max length | Allowed | Case |
| --- | --- | --- | --- |
| Shapefile (DBF) | 10 | Letters, digits, `_`; start with a letter | Stored upper case by most tools |
| File geodatabase | 64 | Letters, digits, `_`; start with a letter; no reserved words | Preserved |
| GeoPackage | No practical limit | Any, but stick to letters, digits, `_` | Preserved; SQLite matches case-insensitively |
| Oracle | 30 (128 from 12.2) | Letters, digits, `_`, `$`, `#` | Upper case unless quoted |
| CSV | None | Any | Preserved; Excel may mangle codes and dates |
