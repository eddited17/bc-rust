//! Report execution and TestPage regression tests on a small self-contained AL app.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session, TestCtx};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 50100 "P3 Item"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Amount; Decimal) { }
        field(3; Processed; Boolean) { }
    }
    keys { key(PK; "No.") { } }
}

table 50101 "P3 Line"
{
    fields
    {
        field(1; "Item No."; Code[20]) { }
        field(2; "Line No."; Integer) { }
        field(3; Qty; Decimal) { }
    }
    keys { key(PK; "Item No.", "Line No.") { } }
}

table 50102 "P3 Log"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; Msg; Text[100]) { }
    }
    keys { key(PK; "Entry No.") { } }
}

report 50100 "P3 Process"
{
    ProcessingOnly = true;

    dataset
    {
        dataitem(Item; "P3 Item")
        {
            DataItemTableView = sorting("No.") where(Amount = filter(> 0));
            RequestFilterFields = "No.";

            dataitem(Line; "P3 Line")
            {
                DataItemLink = "Item No." = field("No.");
                trigger OnAfterGetRecord()
                begin
                    Total += Qty;
                end;
            }

            trigger OnPreDataItem()
            begin
                Log('pre');
            end;

            trigger OnAfterGetRecord()
            begin
                if "No." = 'SKIP' then
                    CurrReport.Skip();
                if "No." = 'STOP' then
                    CurrReport.Break();
                if "No." = 'QUIT' then
                    CurrReport.Quit();
                Processed := true;
                Modify();
                Log('item ' + "No.");
            end;

            trigger OnPostDataItem()
            begin
                Log('post');
            end;
        }
    }

    requestpage
    {
        layout
        {
            area(content)
            {
                field(FactorCtl; Factor) { }
            }
        }
        trigger OnOpenPage()
        begin
            Factor := 2;
        end;
    }

    trigger OnInitReport()
    begin
        Log('init');
    end;

    trigger OnPreReport()
    begin
        Log('prereport');
    end;

    trigger OnPostReport()
    begin
        Log('total ' + Format(Total * Factor));
    end;

    var
        Total: Decimal;
        Factor: Integer;

    procedure SetFactor(F: Integer)
    begin
        Factor := F;
    end;

    local procedure Log(T: Text)
    var
        L: Record "P3 Log";
    begin
        if L.FindLast() then;
        L."Entry No." += 1;
        L.Msg := CopyStr(T, 1, 100);
        L.Insert();
    end;
}

page 50100 "P3 Item Card"
{
    PageType = Card;
    SourceTable = "P3 Item";

    layout
    {
        area(content)
        {
            field("No."; Rec."No.") { }
            field(Amount; Rec.Amount) { }
        }
    }

    actions
    {
        area(processing)
        {
            action(OpenLines)
            {
                RunObject = Page "P3 Lines";
                RunPageLink = "Item No." = field("No.");
                RunPageMode = Create;
            }
        }
    }

    trigger OnInsertRecord(BelowxRec: Boolean): Boolean
    begin
        Rec.Amount := 42;
    end;
}

page 50101 "P3 Lines"
{
    PageType = Card;
    SourceTable = "P3 Line";

    layout
    {
        area(content)
        {
            field("Item No."; Rec."Item No.") { }
            field(Qty; Rec.Qty) { }
        }
    }
}

codeunit 50100 "P3 Fixture"
{
    procedure Setup()
    var
        I: Record "P3 Item";
        L: Record "P3 Line";
        Lg: Record "P3 Log";
    begin
        I.DeleteAll();
        L.DeleteAll();
        Lg.DeleteAll();
        AddItem('A', 1);
        AddItem('B', 1);
        AddItem('C', 0);
        AddItem('SKIP', 1);
        AddItem('STOP', 1);
        AddItem('Z', 1);
        AddLine('A', 10000, 3);
        AddLine('A', 20000, 4);
        AddLine('B', 10000, 50);
        AddLine('Z', 10000, 100);
    end;

    local procedure AddItem(No: Code[20]; Amt: Decimal)
    var
        I: Record "P3 Item";
    begin
        I."No." := No;
        I.Amount := Amt;
        I.Insert();
    end;

    local procedure AddLine(No: Code[20]; LineNo: Integer; Q: Decimal)
    var
        L: Record "P3 Line";
    begin
        L."Item No." := No;
        L."Line No." := LineNo;
        L.Qty := Q;
        L.Insert();
    end;

    procedure LogText(): Text
    var
        Lg: Record "P3 Log";
        T: Text;
    begin
        if Lg.FindSet() then
            repeat
                T += Lg.Msg + '|';
            until Lg.Next() = 0;
        exit(T);
    end;

    procedure Expect(Want: Text)
    var
        Got: Text;
    begin
        Got := LogText();
        if Got <> Want then
            Error('log: expected [%1] got [%2]', Want, Got);
    end;
}

codeunit 50101 "P3 Runs"
{
    trigger OnRun()
    var
        Fx: Codeunit "P3 Fixture";
        I: Record "P3 Item";
        Rep: Report "P3 Process";
        Xml: Text;
    begin
        // the data item tree: view (Amount > 0), caller's view (No. <> B), link, Skip, Break
        Fx.Setup();
        I.SetFilter("No.", '<>B');
        Rep.SetTableView(I);
        Rep.UseRequestPage(false);
        Rep.SetFactor(10);
        Rep.Run();
        Fx.Expect('init|prereport|pre|item A|post|total 70|');
        I.Reset();
        I.Get('A');
        I.TestField(Processed, true);
        I.Get('Z');
        I.TestField(Processed, false);
        I.Get('SKIP');
        I.TestField(Processed, false);

        // Execute with request page parameters: option value and data item view
        Fx.Setup();
        Report.Execute(Report::"P3 Process", '<?xml version="1.0" standalone="yes"?><ReportParameters><Options><Field name="Factor">3</Field></Options><DataItems><DataItem name="Item">SORTING("No.") WHERE("No."=FILTER(A))</DataItem></DataItems></ReportParameters>');
        Fx.Expect('init|prereport|pre|item A|post|total 21|');

        // RunRequestPage returns the parameters (no client outside tests: OK)
        Xml := Report.RunRequestPage(Report::"P3 Process");
        if StrPos(Xml, '<Field name="Factor">') = 0 then
            Error('RunRequestPage: %1', Xml);

        // UseRequestPage := false (property form) and Quit: OnPostReport does not run
        Fx.Setup();
        I.Reset();
        I.Get('B');
        I."No." := 'QUIT';
        I.Insert();
        Clear(Rep);
        Rep.UseRequestPage := false;
        Rep.Run();
        Fx.Expect('init|prereport|pre|item A|item B|');
    end;
}

codeunit 50102 "P3 Tests"
{
    Subtype = Test;

    [Test]
    [HandlerFunctions('ReqHandler')]
    procedure RequestPageHandlerDrivesReport()
    var
        Fx: Codeunit "P3 Fixture";
        Rep: Report "P3 Process";
    begin
        Fx.Setup();
        Rep.Run();
        // OnOpenPage set Factor 2, the handler 5; the data item filter No. = A
        Fx.Expect('init|prereport|pre|item A|post|total 35|');
    end;

    [RequestPageHandler]
    procedure ReqHandler(var Req: TestRequestPage "P3 Process")
    begin
        if Req.FactorCtl.AsInteger() <> 2 then
            Error('OnOpenPage did not run');
        Req.FactorCtl.SetValue(5);
        Req.Item.SetFilter("No.", 'A');
        Req.OK().Invoke();
    end;

    [Test]
    [HandlerFunctions('SaveXmlHandler')]
    procedure FlatDatasetFromTestRequestPage()
    var
        Fx: Codeunit "P3 Fixture";
        Store: Codeunit "P4 Store";
        Rep: Report "P4 Dataset";
    begin
        Fx.Setup();
        Store.SetNames(Store.TempName('.xml'), Store.TempName('.xml'));
        Rep.Run();
        Store.Load();
    end;

    [RequestPageHandler]
    procedure SaveXmlHandler(var Req: TestRequestPage "P4 Dataset")
    var
        Store: Codeunit "P4 Store";
    begin
        Req.SaveAsXml(Store.ParamsName(), Store.DataName());
    end;

    [Test]
    procedure OnInsertRecordWithoutExitInserts()
    var
        I: Record "P3 Item";
        Card: TestPage "P3 Item Card";
    begin
        Card.OpenNew();
        Card."No.".SetValue('NEW');
        Card.Close();
        I.Get('NEW');
        I.TestField(Amount, 42);
    end;

    [Test]
    procedure RunPageModeCreateWithLink()
    var
        I: Record "P3 Item";
        Card: TestPage "P3 Item Card";
        Lines: TestPage "P3 Lines";
    begin
        I."No." := 'K';
        I.Insert();
        Card.OpenEdit();
        Card.GoToRecord(I);
        Lines.Trap();
        Card.OpenLines.Invoke();
        Lines."Item No.".AssertEquals('K');
        Lines.Qty.SetValue(7);
        Lines.Close();
    end;
}

table 50110 "P4 Blob"
{
    fields
    {
        field(1; "Key"; Integer) { }
        field(2; Data; Blob) { }
    }
    keys { key(PK; "Key") { } }
}

report 50110 "P4 Dataset"
{
    dataset
    {
        dataitem(Item; "P3 Item")
        {
            DataItemTableView = sorting("No.");
            RequestFilterFields = "No.";
            column(No; "No.") { IncludeCaption = true; }
            column(Amount; Amount) { AutoFormatType = 1; }
            column(Processed; Processed) { }
            column(Total; Total) { }
            column(Kind; Kind) { }
            column(DocDate; DocDate) { }
            dataitem(Line; "P3 Line")
            {
                DataItemLink = "Item No." = field("No.");
                column(LineNo; "Line No.") { }
                column(Qty; Qty) { DecimalPlaces = 0 : 5; }
                trigger OnAfterGetRecord()
                begin
                    Total += Qty;
                end;
            }
        }
        dataitem(Lonely; "P3 Item")
        {
            DataItemTableView = where("No." = const('C'));
            PrintOnlyIfDetail = true;
            column(LonelyNo; "No.") { }
            dataitem(LonelyLine; "P3 Line")
            {
                DataItemLink = "Item No." = field("No.");
                column(LonelyQty; Qty) { }
            }
        }
    }

    labels
    {
        TitleLbl = 'Items & Lines', Comment = 'title';
    }

    trigger OnPreReport()
    begin
        Kind := Kind::Second;
        DocDate := 20260125D;
    end;

    var
        Total: Decimal;
        Kind: Option First,Second;
        DocDate: Date;
}

codeunit 50110 "P4 Runs"
{
    procedure TreeXml(): Text
    var
        Fx: Codeunit "P3 Fixture";
        B: Record "P4 Blob";
        OutS: OutStream;
        InS: InStream;
        T: Text;
    begin
        Fx.Setup();
        B.Data.CreateOutStream(OutS);
        Report.SaveAs(Report::"P4 Dataset", '', ReportFormat::Xml, OutS);
        B.Data.CreateInStream(InS);
        InS.Read(T);
        exit(T);
    end;

    procedure FileXml(): Text
    var
        Fx: Codeunit "P3 Fixture";
        Store: Codeunit "P4 Store";
        F: File;
        InS: InStream;
        Name: Text;
        T: Text;
    begin
        Fx.Setup();
        Name := Store.TempName('.xml');
        if FILE.Exists(Name) then
            Error('temp name exists');
        Report.SaveAsXml(Report::"P4 Dataset", Name);
        if not FILE.Exists(Name) then
            Error('SaveAsXml wrote no file');
        F.Open(Name);
        F.CreateInStream(InS);
        InS.Read(T);
        F.Close();
        Erase(Name);
        if FILE.Exists(Name) then
            Error('Erase kept the file');
        exit(T);
    end;
}

codeunit 50112 "P4 Library Reads"
{
    // reads the datasets the way Microsoft's "Library - Report Dataset" does
    procedure TreeSummary(): Text
    var
        Runs: Codeunit "P4 Runs";
        Doc: XmlDocument;
        Rows: XmlNodeList;
        Results: XmlNodeList;
        Row: XmlNode;
        Result: XmlNode;
        Qty: Decimal;
        Total: Decimal;
        Lines: Integer;
    begin
        XmlDocument.ReadFrom(Runs.TreeXml(), Doc);
        Doc.SelectNodes('ReportDataSet/DataItems/DataItem', Rows);
        foreach Row in Rows do
            if Row.SelectNodes(StrSubstNo('.//Column[@name="%1"]', 'Qty'), Results) then
                foreach Result in Results do begin
                    Evaluate(Qty, Result.AsXmlElement().InnerText());
                    Total += Qty;
                    Lines += 1;
                end;
        exit(StrSubstNo('rows=%1 lines=%2 qty=%3', Rows.Count(), Lines, Format(Total, 0, 9)));
    end;

    procedure FlatSummary(DataXml: Text): Text
    var
        Doc: XmlDocument;
        Ns: XmlNamespaceManager;
        Schema: XmlNodeList;
        Rows: XmlNodeList;
        Node: XmlNode;
        Attr: XmlAttribute;
        QtyType: Text;
    begin
        XmlDocument.ReadFrom(DataXml, Doc);
        Ns.NameTable(Doc.NameTable);
        Ns.AddNamespace('xs', 'http://www.w3.org/2001/XMLSchema');
        Doc.SelectNodes('//xs:element', Ns, Schema);
        foreach Node in Schema do begin
            Node.AsXmlElement().Attributes().Get('name', Attr);
            if Attr.Value() = 'Qty' then
                if Node.AsXmlElement().Attributes().Get('type', Attr) then
                    QtyType := Attr.Value();
        end;
        Doc.SelectNodes('DataSet/Result', Rows);
        exit(StrSubstNo('schema=%1 rows=%2 qty=%3', Schema.Count(), Rows.Count(), QtyType));
    end;
}

codeunit 50113 "P4 Formats"
{
    // Format(Number, 0, FormatString) as the Standard reports use it (FormatDocument / AutoFormat)
    procedure Check()
    var
        D: Decimal;
        I: Integer;
    begin
        D := 1.08;
        Expect(Format(D, 0, '<C,USD><Precision,2:2><Standard Format,0>'), '1.08');
        Expect(Format(D, 0, '<S,$><Precision,2:5><Standard Format,0>'), '1.08');
        D := 1234.5;
        Expect(Format(D, 0, '<Precision,2:2><Standard Format,0>'), '1,234.50');
        Expect(Format(D, 0, '<Precision,2:2><Standard Format,1>'), '1234.50');
        D := -1234.5678;
        Expect(Format(D, 0, '<Precision,2:2><Standard Format,0>'), '-1,234.57');
        Expect(Format(D, 0, '<Precision,0:0><Standard Format,3>'), '1,235-');
        D := 0.005;
        Expect(Format(D, 0, '<Precision,2:2><Standard Format,0>'), '0.01');
        D := 3;
        Expect(Format(D, 0, '<Precision,0:5><Standard Format,0>'), '3');
        Expect(Format(D, 0, 'Qty <Integer>!'), 'Qty 3!');
        I := 1234567;
        Expect(Format(I, 0, '<Standard Format,0>'), '1,234,567');
        Expect(Format(I, 0, '<Integer>'), '1234567');
    end;

    local procedure Expect(Got: Text; Want: Text)
    begin
        if Got <> Want then
            Error('Format: expected [%1] got [%2]', Want, Got);
    end;
}

codeunit 2000000005 "Reporting Triggers"
{
    // the platform codeunit's event (platform/system/System.Environment.al)
    [BusinessEvent(false)]
    procedure SubstituteReport(ReportId: Integer; RunMode: Option Normal,ParametersOnly,Execute,Print,SaveAs,RunModal; RequestPageXml: Text; RecordRef: RecordRef; var NewReportId: Integer)
    begin
    end;
}

report 50120 "P4 Substitute"
{
    ProcessingOnly = true;
    UseRequestPage = false;

    dataset
    {
        dataitem(Item; "P3 Item")
        {
            trigger OnPreDataItem()
            var
                L: Codeunit "P4 Subst Log";
            begin
                L.Note('ran ' + GetFilters());
                CurrReport.Break();
            end;
        }
    }
}

codeunit 50120 "P4 Subst Log"
{
    SingleInstance = true;

    var
        Seen: Text;

    procedure Note(T: Text)
    begin
        Seen += T + '|';
    end;

    procedure Expect(Want: Text)
    begin
        if Seen <> Want then
            Error('substitution: expected [%1] got [%2]', Want, Seen);
    end;
}

codeunit 50121 "P4 Subst"
{
    EventSubscriberInstance = Manual;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Reporting Triggers", 'SubstituteReport', '', false, false)]
    local procedure OnSubstituteReport(ReportId: Integer; RunMode: Option Normal,ParametersOnly,Execute,Print,SaveAs,RunModal; RequestPageXml: Text; RecordRef: RecordRef; var NewReportId: Integer)
    var
        L: Codeunit "P4 Subst Log";
        T: Text;
    begin
        // (Format of an Option parameter gives its ordinal in bc-rust, so name the modes here)
        case RunMode of
            RunMode::Normal:
                T := 'Normal';
            RunMode::Execute:
                T := 'Execute';
            RunMode::RunModal:
                T := 'RunModal';
            else
                T := 'Other';
        end;
        T += ':' + Format(ReportId);
        if RequestPageXml <> '' then
            T += ':xml';
        if RecordRef.Number = Database::"P3 Item" then
            T += ':' + RecordRef.GetFilters();
        L.Note(T);
        if (ReportId = Report::"P3 Process") and (RunMode in [RunMode::Normal, RunMode::RunModal]) then
            NewReportId := Report::"P4 Substitute";
    end;
}

codeunit 50122 "P4 Subst Runs"
{
    trigger OnRun()
    var
        Fx: Codeunit "P3 Fixture";
        Subst: Codeunit "P4 Subst";
        L: Codeunit "P4 Subst Log";
        I: Record "P3 Item";
        Lg: Record "P3 Log";
        Rep: Report "P3 Process";
    begin
        Fx.Setup();
        // nothing bound: no substitution
        if Report.GetSubstituteReportId(Report::"P3 Process") <> Report::"P3 Process" then
            Error('substituted without a subscriber');
        BindSubscription(Subst);
        if Report.GetSubstituteReportId(Report::"P3 Process") <> Report::"P4 Substitute" then
            Error('GetSubstituteReportId: %1', Report.GetSubstituteReportId(Report::"P3 Process"));
        I.SetRange("No.", 'A');
        // Run / RunModal: the substitute runs, on the caller's record view
        Report.Run(Report::"P3 Process", false, false, I);
        Report.RunModal(Report::"P3 Process", false);
        // Execute: raised with the parameters, but this subscriber keeps the report
        Report.Execute(Report::"P3 Process", '<?xml version="1.0" standalone="yes"?><ReportParameters><Options><Field name="Factor">3</Field></Options><DataItems><DataItem name="Item">SORTING("No.") WHERE("No."=FILTER(A))</DataItem></DataItems></ReportParameters>');
        // a report variable is never substituted (and raises nothing)
        Rep.UseRequestPage(false);
        Rep.Run();
        UnbindSubscription(Subst);
        L.Expect('Normal:50100|Normal:50100:No.: A|ran No.: A|RunModal:50100|ran |Execute:50100:xml|');
        Lg.SetRange(Msg, 'init');
        if Lg.Count() <> 2 then
            Error('P3 Process ran %1 times, expected 2 (Execute and the report variable)', Lg.Count());
    end;
}

codeunit 50111 "P4 Store"
{
    SingleInstance = true;

    var
        ParamsFile: Text;
        DataFile: Text;
        ParamsXml: Text;
        DataXml: Text;

    procedure TempName(Ext: Text): Text
    var
        F: File;
        N: Text;
    begin
        F.CreateTempFile();
        N := F.Name + Ext;
        F.Close();
        exit(N);
    end;

    procedure SetNames(P: Text; D: Text)
    begin
        ParamsFile := P;
        DataFile := D;
    end;

    procedure ParamsName(): Text
    begin
        exit(ParamsFile);
    end;

    procedure DataName(): Text
    begin
        exit(DataFile);
    end;

    procedure Load()
    begin
        ParamsXml := ReadAll(ParamsFile);
        DataXml := ReadAll(DataFile);
    end;

    local procedure ReadAll(Name: Text): Text
    var
        F: File;
        InS: InStream;
        T: Text;
    begin
        if not FILE.Exists(Name) then
            Error('file %1 was not written', Name);
        F.Open(Name);
        F.CreateInStream(InS);
        InS.Read(T);
        exit(T);
    end;

    procedure GetParamsXml(): Text
    begin
        exit(ParamsXml);
    end;

    procedure GetDataXml(): Text
    begin
        exit(DataXml);
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("p3-report-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn code_of(cat: &Catalog, name: &str) -> u32 {
    cat.code.iter().position(|c| c.name == name).unwrap() as u32
}

fn msg(r: Result<al_runtime::Val, Flow>) -> Result<(), String> {
    match r {
        Ok(_) => Ok(()),
        Err(Flow::Err(e)) => Err(format!("{} {:?}", e.msg, e.stack)),
        Err(_) => Ok(()),
    }
}

#[test]
fn report_model() {
    let cat = catalog();
    let rd = &cat.reports[&50100];
    assert!(rd.processing_only);
    assert_eq!(rd.roots.len(), 1);
    let item = &rd.dataitems[rd.roots[0]];
    assert_eq!(item.name, "Item");
    assert_eq!(item.request_filter_fields, vec!["No.".to_string()]);
    assert_eq!(item.children.len(), 1);
    assert_eq!(rd.dataitems[item.children[0]].links.len(), 1);
    assert!(item.triggers.contains_key("onaftergetrecord"));
    assert!(rd.has_request_page());
    assert!(rd.request_triggers.iter().any(|(n, _)| n == "onopenpage"));
    // data items are record globals of the report
    assert!(cat.code[rd.code as usize].globals.iter().any(|g| g.name == "Line"));
}

#[test]
fn report_runs() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let ci = code_of(cat, "P3 Runs");
    let p = cat.code[ci as usize].trigger("onrun").unwrap();
    msg(s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None)).unwrap();
}

fn call_text(s: &mut Session, cat: &Catalog, code: &str, proc: &str, inst: std::rc::Rc<Instance>) -> String {
    let ci = code_of(cat, code);
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(inst), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

/// Remove the ` systemId="..."` attributes (random SystemIds).
fn strip_system_ids(x: &str) -> String {
    let mut out = String::new();
    let mut rest = x;
    while let Some(i) = rest.find(" systemId=\"") {
        out.push_str(&rest[..i]);
        let after = &rest[i + " systemId=\"".len()..];
        rest = &after[after.find('"').unwrap() + 1..];
    }
    out.push_str(rest);
    out
}

const TREE_HEAD: &str = "<?xml version=\"1.0\" standalone=\"yes\"?>\r
<ReportDataSet name=\"P4 Dataset\" id=\"50110\" language=\"en-US\" formatRegion=\"en-US\">\r
  <Labels>\r
    <Label name=\"TitleLbl\">Items &amp; Lines</Label>\r
    <Label name=\"NoCaption\">No.</Label>\r
  </Labels>\r
  <DataItems>\r
    <DataItem language=\"en-US\" formatRegion=\"en-US\" name=\"Item\" tableId=\"50100\">\r
      <Columns>\r
        <Column name=\"No\">A</Column>\r
        <Column name=\"Amount\" decimalformatter=\"#,##0.00\">1</Column>\r
        <Column name=\"Processed\">False</Column>\r
        <Column name=\"Total\" decimalformatter=\"#,##0.00\">3</Column>\r
        <Column name=\"Kind\">Second</Column>\r
        <Column name=\"DocDate\">1/25/2026</Column>\r
      </Columns>\r
      <DataItems>\r
        <DataItem name=\"Line\" tableId=\"50101\">\r
          <Columns>\r
            <Column name=\"LineNo\">10000</Column>\r
            <Column name=\"Qty\" decimalformatter=\"#,##0.#####\">3</Column>\r
          </Columns>\r
        </DataItem>\r
        <DataItem name=\"Line\" tableId=\"50101\">\r
          <Columns>\r
            <Column name=\"LineNo\">20000</Column>\r
            <Column name=\"Qty\" decimalformatter=\"#,##0.#####\">4</Column>\r
          </Columns>\r
        </DataItem>\r
      </DataItems>\r
    </DataItem>\r";

#[test]
fn dataset_tree_xml() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let runs = code_of(cat, "P4 Runs");
    let tree = call_text(&mut s, cat, "P4 Runs", "TreeXml", Instance::new(runs));
    assert!(tree.contains(" systemId=\""), "{tree}");
    let t = strip_system_ids(&tree);
    assert!(t.starts_with(TREE_HEAD), "{t}");
    // items A, B, C, SKIP, STOP, Z; C/SKIP/STOP have no lines (their row is still written)
    assert_eq!(t.matches("<DataItem language=\"en-US\" formatRegion=\"en-US\" name=\"Item\"").count(), 6, "{t}");
    assert_eq!(t.matches("<DataItem name=\"Line\"").count(), 4, "{t}");
    assert_eq!(t.matches("<DataItems />").count(), 3, "{t}");
    // parent columns are evaluated when the first child row is written (after its OnAfterGetRecord)
    assert!(t.contains("<Column name=\"No\">B</Column>"), "{t}");
    assert!(t.contains("<Column name=\"Total\" decimalformatter=\"#,##0.00\">57</Column>"), "{t}");
    assert!(t.contains("<Column name=\"Total\" decimalformatter=\"#,##0.00\">157</Column>"), "{t}");
    // PrintOnlyIfDetail without detail: nothing
    assert!(!t.contains("Lonely"), "{t}");
    assert!(t.ends_with("    </DataItem>\r\n  </DataItems>\r\n</ReportDataSet>"), "{t}");
    // Report.SaveAsXml to a server file writes the same document
    let file = call_text(&mut s, cat, "P4 Runs", "FileXml", Instance::new(runs));
    assert_eq!(strip_system_ids(&file), t);
}

#[test]
fn dataset_flat_xml() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let ci = code_of(cat, "P3 Tests");
    let pi = cat.code[ci as usize].find("FlatDatasetFromTestRequestPage")[0];
    let mut s = Session::new(&rt, "CRONUS");
    let inst = Instance::new(ci);
    s.test = Some(TestCtx::new(cat, inst.clone(), pi).unwrap());
    msg(s.invoke(ProcRef { code: ci, proc: pi }, vec![], Some(inst), None, None)).unwrap();
    let store = code_of(cat, "P4 Store");
    let si = s.single.get(&store).cloned().expect("P4 Store instance");
    let data = call_text(&mut s, cat, "P4 Store", "GetDataXml", si.clone());
    let params = call_text(&mut s, cat, "P4 Store", "GetParamsXml", si);
    s.rollback();
    let schema: Vec<&str> = data.lines().filter(|l| l.contains("<xs:element name=") && l.contains("type=")).map(|l| l.trim()).collect();
    assert_eq!(
        schema,
        vec![
            "<xs:element name=\"No\" type=\"xs:string\" minOccurs=\"0\" />",
            "<xs:element name=\"Amount\" type=\"xs:decimal\" minOccurs=\"0\" />",
            "<xs:element name=\"AmountFormat\" type=\"xs:string\" minOccurs=\"0\" />",
            "<xs:element name=\"Processed\" type=\"xs:boolean\" minOccurs=\"0\" />",
            "<xs:element name=\"Total\" type=\"xs:decimal\" minOccurs=\"0\" />",
            "<xs:element name=\"TotalFormat\" type=\"xs:string\" minOccurs=\"0\" />",
            "<xs:element name=\"Kind\" type=\"xs:string\" minOccurs=\"0\" />",
            "<xs:element name=\"DocDate\" type=\"xs:dateTime\" minOccurs=\"0\" />",
            "<xs:element name=\"LineNo\" type=\"xs:int\" minOccurs=\"0\" />",
            "<xs:element name=\"Qty\" type=\"xs:decimal\" minOccurs=\"0\" />",
            "<xs:element name=\"QtyFormat\" type=\"xs:string\" minOccurs=\"0\" />",
            "<xs:element name=\"LonelyNo\" type=\"xs:string\" minOccurs=\"0\" />",
            "<xs:element name=\"LonelyQty\" type=\"xs:decimal\" minOccurs=\"0\" />",
            "<xs:element name=\"LonelyQtyFormat\" type=\"xs:string\" minOccurs=\"0\" />",
        ],
        "{data}"
    );
    assert!(data.starts_with("<?xml version=\"1.0\" standalone=\"yes\"?>\r\n<DataSet>\r\n  <xs:schema id=\"DataSet\""), "{data}");
    // one row per line, and one per item without lines (A×2, B, C, SKIP, STOP, Z)
    assert_eq!(data.matches("<Result>").count(), 7, "{data}");
    let first = "  <Result>\r
    <No>A</No>\r
    <Amount>1</Amount>\r
    <AmountFormat>#,##0.00</AmountFormat>\r
    <Processed>false</Processed>\r
    <Total>3</Total>\r
    <TotalFormat>#,##0.00</TotalFormat>\r
    <Kind>Second</Kind>\r
    <DocDate>2026-01-25T00:00:00+00:00</DocDate>\r
    <LineNo>10000</LineNo>\r
    <Qty>3</Qty>\r
    <QtyFormat>#,##0.#####</QtyFormat>\r
  </Result>\r
  <Result>\r
    <No>A</No>\r";
    assert!(data.contains(first), "{data}");
    // a row without detail has only the parent's columns
    assert!(data.contains("    <No>C</No>\r\n    <Amount>0</Amount>\r\n"), "{data}");
    assert!(data.contains("<DocDate>2026-01-25T00:00:00+00:00</DocDate>\r\n  </Result>"), "{data}");
    assert!(data.ends_with("</DataSet>"), "{data}");
    assert!(params.contains("<ReportParameter>\r\n    <Name>TitleLbl</Name>\r\n    <Values>\r\n      <string>Items &amp; Lines</string>"), "{params}");
    assert!(params.contains("<Name>NoCaption</Name>"), "{params}");
}

/// The datasets read back through AL's Xml types (as Library - Report Dataset reads them).
#[test]
fn dataset_xml_read_back_in_al() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let lib = code_of(cat, "P4 Library Reads");
    assert_eq!(call_text(&mut s, cat, "P4 Library Reads", "TreeSummary", Instance::new(lib)), "rows=6 lines=4 qty=157");
    // flat: the dataset of the TestRequestPage test
    let ci = code_of(cat, "P3 Tests");
    let pi = cat.code[ci as usize].find("FlatDatasetFromTestRequestPage")[0];
    let inst = Instance::new(ci);
    s.test = Some(TestCtx::new(cat, inst.clone(), pi).unwrap());
    msg(s.invoke(ProcRef { code: ci, proc: pi }, vec![], Some(inst), None, None)).unwrap();
    s.test = None;
    let store = code_of(cat, "P4 Store");
    let si = s.single.get(&store).cloned().unwrap();
    let data = call_text(&mut s, cat, "P4 Store", "GetDataXml", si);
    let lp = cat.code[lib as usize].find("FlatSummary")[0];
    let r = s.invoke(ProcRef { code: lib, proc: lp }, vec![al_runtime::interp::Arg::Val(al_runtime::Val::Text(data))], Some(Instance::new(lib)), None, None);
    let text = match r {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{} {:?}", e.msg, e.stack),
        Err(_) => panic!("flow"),
    };
    // 14 columns + the DataSet and Result elements
    assert_eq!(text, "schema=16 rows=7 qty=xs:decimal");
    s.rollback();
}

#[test]
fn substitute_report_event() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let ci = code_of(cat, "P4 Subst Runs");
    let p = cat.code[ci as usize].trigger("onrun").unwrap();
    msg(s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None)).unwrap();
}

#[test]
fn format_number_pictures() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let ci = code_of(cat, "P4 Formats");
    call_text(&mut s, cat, "P4 Formats", "Check", Instance::new(ci));
}

#[test]
fn dotnet_format_strings() {
    use al_runtime::report::dotnet_format;
    assert_eq!(dotnet_format("<Precision,2:2><Standard Format,0>"), "#,##0.00");
    assert_eq!(dotnet_format("<Precision,2:5><Standard Format,0>"), "#,##0.00###");
    assert_eq!(dotnet_format("<Precision,0:5><Standard Format,0>"), "#,##0.#####");
    assert_eq!(dotnet_format("<Precision,1:3><Standard Format,2>"), "###0.0##");
    assert_eq!(dotnet_format("<Precision,2:2><Standard Format,3>"), "#,##0.00;#,##0.00-");
    assert_eq!(dotnet_format("$<Precision,2:2><Standard Format,1> USD"), "$###0.00 USD;$-###0.00 USD");
    assert_eq!(dotnet_format("<S,EUR><Precision,2:2><Standard Format,0>"), "#,##0.00");
    assert_eq!(dotnet_format("<FormatString>0.0%"), "0.0%");
    assert_eq!(dotnet_format(""), "#,##0.00");
}

#[test]
fn test_codeunit_handlers() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let ci = code_of(cat, "P3 Tests");
    let co = &cat.code[ci as usize];
    for (pi, pr) in co.procs.iter().enumerate() {
        if !pr.attributes.iter().any(|a| a.name.eq_ignore_ascii_case("test")) {
            continue;
        }
        let mut s = Session::new(&rt, "CRONUS");
        let inst = Instance::new(ci);
        s.test = Some(TestCtx::new(cat, inst.clone(), pi as u32).unwrap());
        let r = msg(s.invoke(ProcRef { code: ci, proc: pi as u32 }, vec![], Some(inst), None, None));
        let missing = s.test.as_ref().unwrap().not_executed();
        s.rollback();
        assert_eq!(r, Ok(()), "{}", pr.name);
        assert!(missing.is_empty(), "{}: handlers not executed {missing:?}", pr.name);
    }
}
