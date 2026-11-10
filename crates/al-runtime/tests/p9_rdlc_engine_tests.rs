//! P9: RDLC layout engine stages 2-5 (expressions, binding, page layout, Excel cells).
use al_runtime::rdlc_bind::{self, Dataset, RunState};
use al_runtime::rdlc_expr::{self as ex, Ctx, Expr, V};
use al_runtime::{rdlc, rdlc_xlsx};

struct Fixed(Vec<(&'static str, V)>);
impl Ctx for Fixed {
    fn field(&mut self, name: &str) -> Option<V> {
        self.0.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
    }
    fn param(&mut self, _: &str) -> V {
        V::Text("Cap".into())
    }
    fn report_item(&mut self, _: &str) -> V {
        V::Null
    }
    fn global(&mut self, _: &str) -> V {
        V::Num(1.0)
    }
    fn aggregate(&mut self, func: &str, arg: Option<&Expr>, _: Option<&str>) -> Result<V, String> {
        let v = ex::eval(arg.unwrap(), self)?;
        Ok(match func {
            "sum" => V::Num(v.as_num() * 2.0),
            _ => v,
        })
    }
    fn code(&mut self, name: &str, _: Vec<V>) -> Result<V, String> {
        Err(format!("no {name}"))
    }
}

fn ev(src: &str) -> V {
    let mut cx = Fixed(vec![("No", V::Text("A1".into())), ("Amt", V::Text("1.5".into())), ("Blank", V::Text(String::new()))]);
    ex::eval(&ex::parse(src).unwrap(), &mut cx).unwrap()
}

#[test]
fn expressions() {
    assert_eq!(ev("literal text"), V::Text("literal text".into()));
    assert_eq!(ev("=Fields!No.Value & \"-\" & Parameters!X.Value"), V::Text("A1-Cap".into()));
    assert_eq!(ev("=Fields!Amt.Value * 2 + 1"), V::Num(4.0));
    // VB +: a String operand concatenates (Nothing as ""), numbers add
    assert_eq!(ev("=Nothing + Chr(177) + \"x\""), V::Text("\u{b1}x".into()));
    assert_eq!(ev("=\"5\" + Nothing"), V::Text("5".into()));
    assert_eq!(ev("=1 + \"2\""), V::Num(3.0));
    // VB Replace: an empty Find returns the text unchanged
    assert_eq!(ev("=Replace(\"Addition in Period\", \"\", Chr(10))"), V::Text("Addition in Period".into()));
    // VB strings: a backslash is literal, "" is a quote (BC's captions use \ for line breaks)
    assert_eq!(ev(r#"=Replace("Quantity\Shipped","\",chr(10))"#), V::Text("Quantity\nShipped".into()));
    assert_eq!(ev(r#"="say ""hi""""#), V::Text("say \"hi\"".into()));
    assert_eq!(ev("=IIf(Fields!Blank.Value = \"\", \"empty\", \"set\")"), V::Text("empty".into()));
    assert_eq!(ev("=Sum(Fields!Amt.Value)"), V::Num(3.0));
    assert_eq!(ev("=Not (Fields!Amt.Value > 2) And True"), V::Bool(true));
    assert_eq!(ev("=Format(1234.5, \"N2\")"), V::Text("1,234.50".into()));
    assert_eq!(ev("=Fields!Missing.IsMissing"), V::Bool(true));
    assert_eq!(ev("=Left(\"abcdef\", 3) & Mid(\"abcdef\", 5) & Len(\"xy\")"), V::Text("abcef2".into()));
}

const TABLIX: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Report xmlns="http://schemas.microsoft.com/sqlserver/reporting/2016/01/reportdefinition">
  <ReportSections><ReportSection><Body><ReportItems>
    <Tablix Name="T">
      <TablixBody>
        <TablixColumns><TablixColumn><Width>2cm</Width></TablixColumn><TablixColumn><Width>3cm</Width></TablixColumn></TablixColumns>
        <TablixRows>
          <TablixRow><Height>0.5cm</Height><TablixCells>
            <TablixCell><CellContents><Textbox Name="H1"><Paragraphs><Paragraph><TextRuns><TextRun><Value>No</Value></TextRun></TextRuns></Paragraph></Paragraphs></Textbox></CellContents></TablixCell>
            <TablixCell><CellContents><Textbox Name="H2"><Paragraphs><Paragraph><TextRuns><TextRun><Value>=Parameters!AmtCaption.Value</Value></TextRun></TextRuns></Paragraph></Paragraphs></Textbox></CellContents></TablixCell>
          </TablixCells></TablixRow>
          <TablixRow><Height>0.5cm</Height><TablixCells>
            <TablixCell><CellContents><Textbox Name="D1"><Paragraphs><Paragraph><TextRuns><TextRun><Value>=Fields!No.Value</Value></TextRun></TextRuns></Paragraph></Paragraphs></Textbox></CellContents></TablixCell>
            <TablixCell><CellContents><Textbox Name="D2"><Paragraphs><Paragraph><TextRuns><TextRun><Value>=Code.BlankZero(Fields!Amt.Value)</Value></TextRun></TextRuns></Paragraph></Paragraphs></Textbox></CellContents></TablixCell>
          </TablixCells></TablixRow>
          <TablixRow><Height>0.5cm</Height><TablixCells>
            <TablixCell><CellContents><Textbox Name="S1"><Paragraphs><Paragraph><TextRuns><TextRun><Value>Total</Value></TextRun></TextRuns></Paragraph></Paragraphs></Textbox></CellContents></TablixCell>
            <TablixCell><CellContents><Textbox Name="S2"><Paragraphs><Paragraph><TextRuns><TextRun><Value>=Sum(Fields!Amt.Value)</Value></TextRun></TextRuns></Paragraph></Paragraphs></Textbox></CellContents></TablixCell>
          </TablixCells></TablixRow>
        </TablixRows>
      </TablixBody>
      <TablixRowHierarchy><TablixMembers>
        <TablixMember />
        <TablixMember><Group Name="Details" /></TablixMember>
        <TablixMember><Visibility><Hidden>=Sum(Fields!Amt.Value) = 0</Hidden></Visibility></TablixMember>
      </TablixMembers></TablixRowHierarchy>
      <DataSetName>DataSet_Result</DataSetName>
      <Top>0cm</Top><Left>0cm</Left><Height>1.5cm</Height><Width>5cm</Width>
    </Tablix>
  </ReportItems><Height>1.5cm</Height></Body><Width>5cm</Width></ReportSection></ReportSections>
  <Code>Public Function BlankZero(ByVal Value As Decimal)
    if Value = 0 then
        Return ""
    end if
    Return Value
End Function</Code>
</Report>"#;

#[test]
fn tablix_to_cells() {
    let layout = rdlc::parse(TABLIX).unwrap();
    let ds = Dataset::new(vec!["No".into(), "Amt".into()], vec![vec![V::Text("A".into()), V::Text("1.5".into())], vec![V::Text("B".into()), V::Text("0".into())]]);
    let mut st = RunState::with_code(&layout.code);
    st.params.insert("AmtCaption".into(), V::Text("Amount".into()));
    let nodes = rdlc_bind::bind_body(&layout, &ds, &mut st).unwrap();
    let mut placed = vec![];
    rdlc_xlsx::place(&nodes, 0, 0, &mut placed);
    let sheet = rdlc_xlsx::sheet(&placed);
    let cells: Vec<(String, String)> = sheet.cells.iter().map(|c| (rdlc_xlsx::cell_ref(c.row, c.col), c.value.as_text())).collect();
    // header row, two detail rows (B's amount blanked by Code.BlankZero), the total row
    assert_eq!(
        cells,
        vec![
            ("A1".to_string(), "No".to_string()),
            ("B1".into(), "Amount".into()),
            ("A2".into(), "A".into()),
            ("B2".into(), "1.5".into()),
            ("A3".into(), "B".into()),
            ("A4".into(), "Total".into()),
            ("B4".into(), "1.5".into()),
        ]
    );
    // the 2cm and 3cm columns, in twips
    // the 2cm and 3cm columns: edges at round(1133.86) and round(2834.65) twips
    // tablix edges: Math.Round of the unrounded running sums (1133.86, 2834.65)
    assert_eq!(sheet.col_widths, vec![1134, 2835 - 1134]);
}

#[test]
fn layout_code_interpreted() {
    // functions from BCApps report layouts (SetRowColor, CalcPct, HideOnce, IsEmpty, GetPageCaption), as written
    // there [src:X29]; the declarations at the top are the test's own
    let code = r#"
Public RowColor As Boolean = False
Shared PageCaption As String
Dim hide As Boolean = True
Dim Pct As Decimal

Public Function SetRowColor(ByVal val As Boolean) As Boolean
    RowColor = val
    Return val
End Function

Public Function GetRowColor() As Boolean
  Return RowColor
End Function

Public Function CalcPct(Amount As Decimal, Profit As Decimal) As Decimal
    If Amount <> 0 Then
       Pct = 100 * Profit / Amount
    Else
       Pct = 0
    End If
    Return Round(10 * Pct) / 10
End Function

Public Function HideOnce As Boolean
  if hide then
    hide = false
    Return true
 end if
 Return false
End Function

Public Function IsEmpty(value as Object) as boolean
  On Error Resume Next
  If Convert.ToBase64String(Value) = "" then
      Return True
  Else
      Return False
  End If
End Function

Public Function SetPageCaption(c As String) : PageCaption = c : End Function
Public Function GetPageCaption
  return CStr(PageCaption) & "!"
End Function

Function Classify(n As Integer) As String
    Select Case n
        Case 1, 2
            Classify = "low"
        Case Else
            Classify = "high"
    End Select
End Function

Function SumTo(n As Integer) As Integer
    Dim i As Integer, s As Integer
    For i = 1 To n
        s += i
    Next i
    SumTo = s
End Function
"#;
    let mut vb = al_runtime::rdlc_vb::VbState::new(code).unwrap();
    let mut cx = Fixed(vec![]);
    let mut call = |n: &str, a: Vec<V>| vb.call(n, a, &mut cx).unwrap();
    assert_eq!(call("getrowcolor", vec![]), V::Bool(false));
    assert_eq!(call("setrowcolor", vec![V::Bool(true)]), V::Bool(true));
    assert_eq!(call("getrowcolor", vec![]), V::Bool(true));
    assert_eq!(call("calcpct", vec![V::Num(300.0), V::Num(100.0)]), V::Num(33.3));
    assert_eq!(call("calcpct", vec![V::Num(0.0), V::Num(100.0)]), V::Num(0.0));
    assert_eq!(call("hideonce", vec![]), V::Bool(true));
    assert_eq!(call("hideonce", vec![]), V::Bool(false));
    assert_eq!(call("isempty", vec![V::Text(String::new())]), V::Bool(true));
    assert_eq!(call("isempty", vec![V::Text("iVBOR".into())]), V::Bool(false));
    call("setpagecaption", vec![V::Text("Page".into())]);
    assert_eq!(call("getpagecaption", vec![]), V::Text("Page!".into()));
    assert_eq!(call("classify", vec![V::Num(2.0)]), V::Text("low".into()));
    assert_eq!(call("classify", vec![V::Num(7.0)]), V::Text("high".into()));
    assert_eq!(call("sumto", vec![V::Num(4.0)]), V::Num(10.0));
}


const PAGED: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Report xmlns="http://schemas.microsoft.com/sqlserver/reporting/2016/01/reportdefinition">
  <ReportSections><ReportSection><Body><ReportItems>
    <Tablix Name="L">
      <TablixBody>
        <TablixColumns><TablixColumn><Width>4cm</Width></TablixColumn></TablixColumns>
        <TablixRows>
          <TablixRow><Height>0.5cm</Height><TablixCells>
            <TablixCell><CellContents><Textbox Name="D"><Paragraphs><Paragraph><TextRuns><TextRun><Value>=Fields!No.Value</Value></TextRun></TextRuns></Paragraph></Paragraphs></Textbox></CellContents></TablixCell>
          </TablixCells></TablixRow>
        </TablixRows>
      </TablixBody>
      <TablixRowHierarchy><TablixMembers>
        <TablixMember><Group Name="Doc"><GroupExpressions><GroupExpression>=Fields!No.Value</GroupExpression></GroupExpressions><PageBreak><BreakLocation>Between</BreakLocation></PageBreak></Group></TablixMember>
      </TablixMembers></TablixRowHierarchy>
      <Top>0cm</Top><Left>0cm</Left><Height>0.5cm</Height><Width>4cm</Width>
    </Tablix>
  </ReportItems><Height>0.5cm</Height></Body>
  <Page><PageHeader><Height>0.5cm</Height><ReportItems>
    <Textbox Name="P"><Paragraphs><Paragraph><TextRuns><TextRun><Value>="Page " &amp; Globals!PageNumber &amp; " of " &amp; Globals!TotalPages</Value></TextRun></TextRuns></Paragraph></Paragraphs>
      <Top>0cm</Top><Left>0cm</Left><Height>0.5cm</Height><Width>4cm</Width></Textbox>
  </ReportItems></PageHeader></Page>
  <Width>4cm</Width></ReportSection></ReportSections>
</Report>"#;

#[test]
fn page_breaks_make_worksheets() {
    // a group's PageBreak Between: one worksheet per document, each with its own page header
    let lines = vec![(0usize, "Doc", vec![("No", "D1".to_string())]), (0, "Doc", vec![("No", "D2".to_string())])];
    let xlsx = rdlc_xlsx::excel(PAGED, &lines, &[], "ADMIN").unwrap();
    let parts = al_runtime::dotnet::zip::read_zip(&xlsx).unwrap();
    let text = |name: &str| String::from_utf8(parts.iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("{name}")).1.clone()).unwrap();
    let (s1, s2) = (text("xl/worksheets/sheet1.xml"), text("xl/worksheets/sheet2.xml"));
    assert!(s1.contains("Page 1 of 2") && s1.contains(">D1<") && !s1.contains(">D2<"), "{s1}");
    assert!(s2.contains("Page 2 of 2") && s2.contains(">D2<") && !s2.contains(">D1<"), "{s2}");
    assert!(!parts.iter().any(|(n, _)| n == "xl/worksheets/sheet3.xml"));
}
