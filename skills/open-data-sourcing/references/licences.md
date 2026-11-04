# Common data licences and what they require

Use SPDX identifiers (spdx.org/licenses) in provenance records, so a licence can be matched exactly. `LicenseRef-...` is the SPDX convention for anything not on the SPDX list.

This is practical guidance, not legal advice. When the stakes are real (redistribution, commercial products, contracts), the person's organization decides.

| SPDX id | Name | Attribution | Share-alike | Commercial use | Notes |
| --- | --- | --- | --- | --- | --- |
| `CC0-1.0` | Creative Commons Zero | Not required (still cite it) | No | Yes | Public-domain dedication |
| `PDDL-1.0` | Open Data Commons Public Domain Dedication and Licence | Not required | No | Yes | The database version of CC0 |
| `LicenseRef-Public-Domain` | Public domain, no licence text | Not required (still cite it) | No | Yes | Typical for US federal works. Check the agency's terms of service for the access method |
| `CC-BY-4.0` | Creative Commons Attribution 4.0 | Yes: credit, link the licence, say what you changed | No | Yes | The most common open licence for government data outside the US |
| `ODC-By-1.0` | Open Data Commons Attribution | Yes | No | Yes | The database version of CC-BY |
| `OGL-UK-3.0` | UK Open Government Licence v3 | Yes, with the publisher's attribution statement | No | Yes | Used by UK public bodies, including Ordnance Survey open products |
| `CC-BY-SA-4.0` | Creative Commons Attribution-ShareAlike 4.0 | Yes | Yes: adaptations under the same licence | Yes | Watch combining it with incompatible licences |
| `ODbL-1.0` | Open Database Licence | Yes: "© OpenStreetMap contributors" for OSM | Yes, for derived databases you make public. A map image (a "produced work") may use any licence but must credit the source | Yes | OpenStreetMap. Mixing OSM with other data into a shared database brings in share-alike |
| `CC-BY-NC-4.0` | Creative Commons Attribution-NonCommercial 4.0 | Yes | No | **No** | Flag it before contracted, fee-based or commercial work |
| `CC-BY-ND-4.0` | Creative Commons Attribution-NoDerivatives 4.0 | Yes | n/a | Yes | No derived data. Clipping or reprojecting may count as an adaptation |
| (none stated) | All rights reserved by default | n/a | n/a | n/a | Not open. Ask the publisher before any use beyond viewing |
| `LicenseRef-...` (custom terms) | A publisher's own terms | Read them | Read them | Read them | Common for state and local data. Look for disclaimers, redistribution limits and "not for survey/legal use" |

## Terms that sit beside the licence

- **Service terms:** an open dataset can be served through an API whose terms limit request rates, require a key, or forbid bulk harvesting. Use the bulk download when one is offered.
- **Basemaps and imagery from commercial vendors** are licensed for display inside their products. Tracing or extracting features from them is usually forbidden.
- **Personal data** in "open" records (owner names in parcel data, for example) can still be restricted by law or policy. Check before you redistribute.

## Attribution text

- Use the publisher's own wording when they give it.
- Otherwise: `<Dataset title>, <Publisher>, <year or as-of date>, <licence name> (<licence URL>). Retrieved <date> from <URL>.`
- Say when you changed the data: "Clipped to Travis County and reprojected to EPSG:3081."
