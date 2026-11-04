# ArcPy errors and fixes

Read the full message with `arcpy.GetMessages(2)` inside `except arcpy.ExecuteError:`.
A plain Python error (`RuntimeError`, `TypeError`) means the problem is in your code or in a
cursor, not in a geoprocessing tool.

| Message | Usual cause | Fix |
| --- | --- | --- |
| ERROR 000725: Output ... already exists | `arcpy.env.overwriteOutput` is False | Set it to True at the top, or delete the output first with `arcpy.management.Delete` |
| ERROR 000732: Input ... does not exist or is not supported | Wrong workspace, typo, relative path, or a map layer name used in a standalone script | Use the full path; check `arcpy.Exists(path)` |
| ERROR 000464: Cannot get exclusive schema lock | Pro, ArcGIS Server or an open cursor is using the dataset | Close the map or layer, stop the service, open cursors in `with` blocks |
| ERROR 000210: Cannot create output | Folder missing, no write permission, invalid name, or output inside a read-only connection | Create the folder; check `arcpy.ValidateTableName`; write to a local geodatabase |
| ERROR 000354: The name contains invalid characters | Spaces, hyphens or a leading digit in a geodatabase name | `arcpy.ValidateTableName(name, workspace)` |
| ERROR 000824: The tool is not licensed | Extension not checked out, or the licence level is too low | `arcpy.CheckOutExtension("Spatial")`; check the tool's Licensing section |
| ERROR 999999: Something unexpected caused the tool to fail | Often invalid geometry, a corrupt dataset or a path too long | Run `arcpy.management.RepairGeometry` on a copy; shorten paths; try a smaller subset |
| RuntimeError: Objects in this class cannot be updated outside an edit session | Versioned, topology, network or relationship-class data edited without an edit session | Wrap the cursor in `with arcpy.da.Editor(workspace, multiuser_mode=True):` |
| RuntimeError: Cannot acquire a lock | Another cursor (often a nested one) or another program holds the table | Read one table into a dict first; do not nest cursors on the same table |
| RuntimeError: A column was specified that does not exist | Field name typo, alias used instead of name, or unqualified name after a join | Print `[f.name for f in arcpy.ListFields(fc)]` |
| RuntimeError: An invalid SQL statement was used | Where clause in the wrong dialect, missing quotes around text, wrong delimiters | Build it with `arcpy.AddFieldDelimiters`; quote text values; test the clause in Pro's Select By Attributes |
| TypeError: sequence size must match size of the row | `updateRow`/`insertRow` given a different number of values than the cursor's fields | Keep the row as returned and change items in place |
| Output is empty, no error | Selection on a layer, an `extent` environment, or a where clause that matches nothing | Pass the feature class path; reset `arcpy.env.extent`; count with the where clause first |
| Coordinates off by about 1 to 2 m (or 100 m with NAD27) | Datum change with no transformation | Set `arcpy.env.geographicTransformations` or the `transform_method` of `arcpy.management.Project` |
| `ModuleNotFoundError` for a package | Script running in a different Python, or the package installed into the wrong environment | Run with `propy.bat`; install into a clone made with `conda create --clone arcgispro-py3` |
