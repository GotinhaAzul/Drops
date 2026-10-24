#!/usr/bin/env python3
"""Regression: cached cells covered by an XLSX array formula are read-only."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('office_helper', ROOT / 'crates/buddy-agent/src/tools/office.py')
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
S = helper.S


class ArrayFormulaTests(unittest.TestCase):
    def workbook(self, base, reference='A1:B1'):
        path = base / 'array.xlsx'
        with zipfile.ZipFile(path, 'w') as archive:
            archive.writestr('xl/workbook.xml', f'<workbook xmlns="{S}" xmlns:r="{helper.R}"><sheets><sheet name="Data" sheetId="1" r:id="r1"/></sheets></workbook>')
            archive.writestr('xl/_rels/workbook.xml.rels', '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="r1" Target="worksheets/sheet.xml"/></Relationships>')
            archive.writestr('xl/worksheets/sheet.xml', f'<worksheet xmlns="{S}"><sheetData><row r="1"><c r="A1"><f t="array" ref="{reference}">ROW(A1:B2)</f><v>1</v></c><c r="B1"><v>1</v></c><c r="C1"><v>10</v></c></row><row r="2"><c r="A2"><v>2</v></c><c r="B2"><v>2</v></c><c r="C2"><v>20</v></c></row></sheetData></worksheet>')
        return path

    def edit(self, base, source, cells):
        fingerprint = helper.run(dict(_root=str(base), path=str(source)))['sha256']
        return helper.run(dict(_root=str(base), _edit=True, path=str(source), output_path=str(base / 'copy.xlsx'), expected_sha256=fingerprint, sheet='Data', cells=cells))

    def test_cached_result_cell_is_rejected_without_publishing_copy(self):
        with tempfile.TemporaryDirectory(prefix='drops-array-test-') as directory:
            base = Path(directory)
            source = self.workbook(base)
            original = source.read_bytes()
            # C1 is editable; the later B1 rejection must prevent the entire copy.
            with self.assertRaisesRegex(ValueError, 'array.formula'):
                self.edit(base, source, [dict(cell='C1', value=99), dict(cell='B1', value=42)])
            self.assertFalse((base / 'copy.xlsx').exists())
            self.assertEqual(source.read_bytes(), original)

    def test_entire_two_dimensional_range_is_read_only(self):
        for reference in ['A1:B2', '$A$1:$B$2']:
            for cell in ['A1', 'B1', 'A2', 'B2']:
                with self.subTest(reference=reference, cell=cell), tempfile.TemporaryDirectory(prefix='drops-array-test-') as directory:
                    base = Path(directory)
                    source = self.workbook(base, reference)
                    with self.assertRaises(ValueError):
                        self.edit(base, source, [dict(cell=cell, value=42)])
                    self.assertFalse((base / 'copy.xlsx').exists())

    def test_single_cell_range_and_invalid_metadata(self):
        for reference in ['A1', '$A$1', '', 'not-a-range', 'B2:A1']:
            with self.subTest(reference=reference), tempfile.TemporaryDirectory(prefix='drops-array-test-') as directory:
                base = Path(directory)
                source = self.workbook(base, reference)
                with self.assertRaisesRegex(ValueError, 'array.formula'):
                    self.edit(base, source, [dict(cell='A1' if reference in ['A1', '$A$1'] else 'C1', value=42)])
                self.assertFalse((base / 'copy.xlsx').exists())

    def test_cells_outside_range_stay_editable_and_formula_is_preserved(self):
        with tempfile.TemporaryDirectory(prefix='drops-array-test-') as directory:
            base = Path(directory)
            source = self.workbook(base, 'A1:B2')
            self.edit(base, source, [dict(cell='C1', value=99)])
            with zipfile.ZipFile(source) as before, zipfile.ZipFile(base / 'copy.xlsx') as after:
                first = helper.xml(before, 'xl/worksheets/sheet.xml')
                second = helper.xml(after, 'xl/worksheets/sheet.xml')
                self.assertEqual(helper.elements(first, S, 'f')[0].toxml(), helper.elements(second, S, 'f')[0].toxml())
            rows = helper.run(dict(_root=str(base), path=str(base / 'copy.xlsx'), sheet='Data'))['cells']
            self.assertEqual(next(c['value'] for c in rows if c['cell'] == 'C1'), '99')
            self.assertEqual(next(c['value'] for c in rows if c['cell'] == 'B1'), '1')


if __name__ == '__main__':
    unittest.main()
