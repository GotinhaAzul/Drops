#!/usr/bin/env python3
"""Real ZIP/XML round trips through the exact embedded office helper; stdlib only."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import zipfile
from xml.dom import minidom

ROOT=Path(__file__).resolve().parents[1]
HELPER=ROOT/'crates/buddy-agent/src/tools/office.py'
W='http://schemas.openxmlformats.org/wordprocessingml/2006/main'
S='http://schemas.openxmlformats.org/spreadsheetml/2006/main'


def main():
    with tempfile.TemporaryDirectory(prefix='drops-office-test-') as directory:
        base=Path(directory)
        def call(path, edit=False, success=True, **args):
            result=subprocess.run([sys.executable,'-I',str(HELPER)],input=json.dumps(dict(_root=str(base),_edit=edit,path=str(path),**args)),text=True,capture_output=True,timeout=20)
            body=json.loads(result.stdout)
            assert (result.returncode==0)==success, body
            return body.get('result',body.get('error'))
        source=base/'report.docx'
        document=f'''<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="{W}" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" mc:Ignorable="w14"><w:body><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Hello </w:t></w:r><w:r><w:t>coworker!</w:t></w:r></w:p><w:p><w:r><w:t>Second paragraph</w:t></w:r></w:p></w:body></w:document>'''
        unchanged=b'pretend-embedded-image'
        with zipfile.ZipFile(source,'w') as archive:
            archive.writestr('word/document.xml',document)
            archive.writestr('word/media/image.png',unchanged)
        original=source.read_bytes()
        read=call(source)
        assert read['paragraphs'][0]['text']=='Hello coworker!'
        assert read['sha256']==hashlib.sha256(original).hexdigest()
        output=base/'report-edited.docx'
        edited=call(source,True,expected_sha256=read['sha256'],output_path=str(output),replacements=[dict(old_text='lo cowork',new_text='pful work')])
        assert edited['output_path']==str(output)
        assert call(output)['paragraphs'][0]['text']=='Helpful worker!'
        with zipfile.ZipFile(output) as archive:
            assert archive.read('word/media/image.png')==unchanged
            dom=minidom.parseString(archive.read('word/document.xml'))
            assert dom.documentElement.getAttribute('xmlns:w14')
            assert len(dom.getElementsByTagNameNS(W,'b'))==1
        assert source.read_bytes()==original
        assert 'exists' in call(source,True,False,expected_sha256=read['sha256'],output_path=str(output),replacements=[dict(old_text='Hello',new_text='Hey')])
        failed=base/'failed.docx'
        assert 'fingerprint' in call(source,True,False,expected_sha256='bad',output_path=str(failed),replacements=[])
        assert not failed.exists()
        assert 'exactly once' in call(source,True,False,expected_sha256=read['sha256'],output_path=str(failed),replacements=[dict(old_text='Hello',new_text='Hey'),dict(old_text='missing',new_text='no')])
        assert not failed.exists()
        assert 'different path' in call(source,True,False,expected_sha256=read['sha256'],output_path=str(source),replacements=[dict(old_text='Hello',new_text='Hey')])
        assert 'outside' in call(Path('/etc/passwd'),success=False)
        workbook=base/'budget.xlsx'
        with zipfile.ZipFile(workbook,'w') as archive:
            archive.writestr('xl/workbook.xml',f'<s:workbook xmlns:s="{S}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><s:sheets><s:sheet name="Budget" sheetId="1" r:id="rId1"/></s:sheets></s:workbook>')
            archive.writestr('xl/_rels/workbook.xml.rels','<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Target="worksheets/sheet1.xml" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"/></Relationships>')
            archive.writestr('xl/worksheets/sheet1.xml',f'<s:worksheet xmlns:s="{S}"><s:sheetData><s:row r="1"><s:c r="A1" t="s" s="2"><s:v>0</s:v></s:c><s:c r="B1"><s:v>10</s:v></s:c><s:c r="C1"><s:f>B1*2</s:f><s:v>20</s:v></s:c></s:row></s:sheetData></s:worksheet>')
            archive.writestr('xl/sharedStrings.xml',f'<sst xmlns="{S}"><si><t>Revenue</t></si></sst>')
            archive.writestr('xl/styles.xml',b'untouched-styles')
        before=workbook.read_bytes()
        read=call(workbook)
        assert read['sheets']==['Budget']
        cells=call(workbook,sheet='Budget')['cells']
        assert cells[0]['value']=='Revenue'
        assert cells[2]['formula']=='B1*2'
        copy=base/'budget-edited.xlsx'
        call(workbook,True,expected_sha256=read['sha256'],output_path=str(copy),sheet='Budget',cells=[dict(cell='A1',value='New revenue'),dict(cell='B1',value=30)])
        cells=call(copy,sheet='Budget')['cells']
        assert cells[0]['value']=='New revenue' and cells[1]['value']=='30'
        assert cells[2]['formula']=='B1*2' and cells[2]['value']=='20'
        assert workbook.read_bytes()==before
        with zipfile.ZipFile(copy) as archive:
            assert archive.read('xl/styles.xml')==b'untouched-styles'
            dom=minidom.parseString(archive.read('xl/worksheets/sheet1.xml'))
            assert dom.getElementsByTagNameNS(S,'c')[0].getAttribute('s')=='2'
            assert dom.getElementsByTagNameNS(S,'is')
        assert 'read-only' in call(workbook,True,False,expected_sha256=read['sha256'],output_path=str(base/'formula.xlsx'),sheet='Budget',cells=[dict(cell='C1',value=42)])
        assert not (base/'formula.xlsx').exists()
        # No XML entity processing, even in an otherwise valid package.
        malicious=base/'unsafe.docx'
        with zipfile.ZipFile(malicious,'w') as archive:
            archive.writestr('word/document.xml','<!DOCTYPE x [<!ENTITY x "unsafe">]><x/>')
        assert 'DTD' in call(malicious,success=False)
    print('PASS: DOCX run-spanning edits, namespace/style/part preservation, XLSX values/formula preservation, fingerprints, confinement, no-overwrite, all-or-nothing validation and XML limits')


if __name__=='__main__':main()
