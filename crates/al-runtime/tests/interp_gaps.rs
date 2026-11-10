//! Interpreter gaps closed by P7: property-assignment procedure calls, CurrFieldNo, the Dialog
//! type, Option metadata (Format / Evaluate), Notification / SessionSettings values, bare
//! implicit-Rec properties, system functions (Sid, table connections, EncryptionEnabled,
//! IsSessionActive) and empty key ranges.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session, TestCtx, Val};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
enum 50700 "P7 Color"
{
    Extensible = true;
    value(0; Red) { Caption = 'Red Caption'; }
    value(5; Green) { Caption = 'Green Caption'; }
}

enum 50790 "P7 Line Type"
{
    value(0; " ") { }
    value(1; Item) { }
    value(2; "Charge (Item)") { }
}

// an enum named like a field of P7 Item: Rec.LineType::X must not resolve to it
enum 50791 LineType
{
    value(0; Other) { }
}

table 50700 "P7 Item"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Amount; Decimal)
        {
            trigger OnValidate()
            begin
                SeenCurrField := CurrFieldNo;
            end;
        }
        field(3; SeenCurrField; Integer) { }
        field(4; Flag; Boolean) { }
        field(5; Kind; Option) { OptionMembers = Normal,Special; OptionCaption = 'Normal Kind,Special Kind'; }
        field(6; Color; Enum "P7 Color") { }
        field(11; "Qty. Count"; Integer) { }
        field(10; LineType; Enum "P7 Line Type") { }
        field(9; DueCalc; DateFormula) { }
        field(7; MinQty; Decimal) { MinValue = 0; }
        field(8; Pct; Integer) { MinValue = -1; MaxValue = 100; }
    }
    keys { key(PK; "No.") { } }

    procedure ScopeFromRec(): Text
    begin
        Rec.LineType := Rec.LineType::"Charge (Item)";
        exit(Format(Rec.LineType = Rec.LineType::"Charge (Item)") + Format(xRec.LineType <> xRec.LineType::Item) + Format(Rec.LineType));
    end;

    procedure SetFlag(Value: Boolean)
    begin
        Flag := Value;
    end;

    procedure SetCurrFieldAndRead(): Integer
    begin
        CurrFieldNo := FieldNo(Amount);
        exit(CurrFieldNo);
    end;

    procedure BareProperties(): Text
    begin
        FilterGroup := 2;
        SetRange(Amount, 1);
        ReadIsolation := IsolationLevel::ReadUncommitted;
        FilterGroup := 0;
        exit(Format(GetFilter(Amount) = ''));
    end;
}

table 2000000068 "Record Link"
{
    fields
    {
        field(1; "Link ID"; Integer) { AutoIncrement = true; }
        field(2; "Record ID"; RecordID) { }
        field(3; URL1; Text[2048]) { }
        field(7; Description; Text[250]) { }
        field(10; Created; DateTime) { }
        field(11; "User ID"; Text[132]) { }
        field(12; Company; Text[30]) { }
    }
    keys { key(PK; "Link ID") { } }
}

codeunit 50700 "P7 Lib"
{
    var
        X: Integer;

    procedure SetX(Value: Integer)
    begin
        X := Value;
    end;

    procedure GetX(): Integer
    begin
        exit(X);
    end;

    procedure ActionHandler(N: Notification)
    begin
    end;
}

codeunit 50702 "P7 Lib2"
{
}

interface "P7 Greeter"
{
    procedure Name(): Text;

    procedure Greet(Who: Text): Text
    begin
        exit('hello ' + Who);
    end;
}

codeunit 50705 "P7 Greeter Impl" implements "P7 Greeter"
{
    procedure Name(): Text
    begin
        exit('impl');
    end;
}

codeunit 50703 "P7 Publisher"
{
    TableNo = "P7 Item";

    var
        Called: Boolean;

    trigger OnRun()
    begin
        OnSomething();
    end;

    procedure MarkCalled()
    begin
        Called := true;
    end;

    procedure WasCalled(): Boolean
    begin
        exit(Called);
    end;

    [IntegrationEvent(true, false)]
    local procedure OnSomething()
    begin
    end;
}

codeunit 50781 "S4 Try Pub"
{
    procedure Send(): Boolean
    begin
        if OnTryPost() then
            exit(true);
        exit(false);
    end;

    [TryFunction]
    [IntegrationEvent(false, false)]
    procedure OnTryPost()
    begin
    end;
}

codeunit 50782 "S4 Try Sub"
{
    EventSubscriberInstance = Manual;

    var
        Fail: Boolean;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"S4 Try Pub", 'OnTryPost', '', false, false)]
    [TryFunction]
    local procedure HandleTryPost()
    begin
        if Fail then
            Error('post failed');
    end;

    procedure SetFail(Value: Boolean)
    begin
        Fail := Value;
    end;
}

codeunit 50707 "P7 Bind Pub"
{
    procedure Ping(): Integer
    var
        Count: Integer;
    begin
        OnPing(Count);
        exit(Count);
    end;

    [IntegrationEvent(false, false)]
    local procedure OnPing(var Count: Integer)
    begin
    end;
}

codeunit 50706 "P7 Manual Sub"
{
    EventSubscriberInstance = Manual;

    var
        Hits: Integer;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"P7 Bind Pub", 'OnPing', '', false, false)]
    local procedure HandlePing(var Count: Integer)
    begin
        Hits += 1;
        Count += 1;
    end;

    procedure GetHits(): Integer
    begin
        exit(Hits);
    end;
}

table 2000000175 "Scheduled Task"
{
    DataPerCompany = false;
    fields
    {
        field(1; ID; Guid) { }
        field(3; "User Name"; Text[50]) { }
        field(10; Company; Text[30]) { }
        field(11; "Is Ready"; Boolean) { }
        field(12; "Not Before"; DateTime) { }
        field(20; "Run Codeunit"; Integer) { }
        field(21; "Failure Codeunit"; Integer) { }
        field(22; "Record"; RecordID) { }
    }
    keys { key(PK; ID) { } }
}

table 2000000192 "Page Control Field"
{
    DataPerCompany = false;
    fields
    {
        field(1; PageNo; Integer) { }
        field(2; ControlId; Integer) { }
        field(3; ControlName; Text[120]) { }
        field(4; TableNo; Integer) { }
        field(5; FieldNo; Integer) { }
        field(6; Enabled; Text[512]) { }
        field(7; Editable; Text[512]) { }
        field(8; Visible; Text[512]) { }
        field(9; SourceExpression; Text[512]) { }
        field(10; OptionString; Text[2048]) { }
        field(11; Sequence; Integer) { }
    }
    keys { key(Key1; PageNo, ControlId) { } }
}

page 50719 "P7 Control Fields"
{
    PageType = Card;
    SourceTable = "P7 Item";

    layout
    {
        area(Content)
        {
            group(General)
            {
                field("Item No."; Rec."No.") { }
                field(KindCtl; Rec.Kind) { Editable = false; }
                field(Note; NoteTxt) { Visible = Rec.Flag; }
            }
            part(Sub; "P7 Item List") { }
        }
    }

    var
        NoteTxt: Text;
}

page 50720 "P7 Close Dlg"
{
    PageType = Card;

    layout
    {
        area(Content)
        {
            field(CloseCtl; CloseTxt)
            {
                trigger OnDrillDown()
                begin
                    CurrPage.Close();
                end;
            }
        }
    }
    actions
    {
        area(Processing)
        {
            action(CloseAct)
            {
                trigger OnAction()
                begin
                    CurrPage.Close();
                end;
            }
        }
    }

    var
        CloseTxt: Text;
}

table 2000000184 "Tenant Media"
{
    DataPerCompany = false;
    fields
    {
        field(1; ID; Guid) { }
        field(2; Content; Blob) { }
        field(3; "Mime Type"; Text[100]) { }
        field(4; Height; Integer) { }
        field(5; Width; Integer) { }
        field(6; "Company Name"; Text[30]) { }
        field(7; Description; Text[250]) { }
        field(8; "Expiration Date"; DateTime) { }
        field(12; "Security Token"; Text[100]) { }
    }
    keys { key(Key1; ID) { } }
}

page 50721 "P7 Other Rec"
{
    PageType = Card;
    SourceTable = "P7 Item";

    layout
    {
        area(Content)
        {
            field(OtherKind; Other.Kind) { }
        }
    }

    trigger OnOpenPage()
    begin
        Other.Kind := Other.Kind::Special;
    end;

    var
        Other: Record "P7 Item";
}

page 50724 "P7 Nav Page"
{
    PageType = Card;
    SourceTable = "P7 Item";

    layout
    {
        area(Content)
        {
            field(GoNo; GoNo)
            {
                trigger OnValidate()
                begin
                    Rec.Get(GoNo);
                end;

                trigger OnLookup(var Text: Text): Boolean
                begin
                    Rec.Amount := 42;
                end;
            }
        }
    }

    trigger OnModifyRecord(): Boolean
    begin
        Rec.Flag := true;
    end;

    var
        GoNo: Code[20];
}

table 50722 "P7 Prio"
{
    fields
    {
        field(1; "Code"; Code[10]) { }
        field(2; Prio; Option) { OptionMembers = Normal,"Very Low (Hidden)"; }
    }
    keys { key(PK; "Code") { } }
}

page 50722 "P7 Prio List"
{
    PageType = List;
    SourceTable = "P7 Prio";
    SourceTableView = where(Prio = filter(<> "Very Low (Hidden)"));

    layout
    {
        area(Content)
        {
            repeater(Lines)
            {
                field("Code"; Rec."Code") { }
            }
        }
    }
}

table 50723 "P7 RecLink"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; "Rec ID"; RecordId) { }
    }
    keys { key(PK; "Entry No.") { } }
}

table 50725 "P7 RidBuf"
{
    fields
    {
        field(1; ID; Integer) { }
        field(14; RecordID; RecordID) { }
    }
    keys { key(PK; ID) { } }
}

codeunit 50708 "P7 Task"
{
    TableNo = "P7 Item";

    trigger OnRun()
    begin
        Rec.Amount += 1;
        Rec.Modify();
        if Rec.Flag then
            Error('task failed');
    end;
}

codeunit 50709 "P7 Task Failure"
{
    TableNo = "P7 Item";

    trigger OnRun()
    begin
        Rec."No." := 'FAILED';
        Rec.Amount := StrLen(GetLastErrorText());
        Rec.Insert();
    end;
}

codeunit 50704 "P7 Subscriber"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"P7 Publisher", 'OnSomething', '', false, false)]
    local procedure HandleOnSomething(sender: Codeunit "P7 Publisher")
    begin
        sender.MarkCalled();
    end;
}

page 50700 "P7 Page"
{
    PageType = Card;
    SourceTable = "P7 Item";
}

page 50701 "P7 Vis Page"
{
    PageType = Card;
    SourceTable = "P7 Item";

    layout
    {
        area(Content)
        {
            field(Flag; Rec.Flag)
            {
                Visible = Rec."Qty. Count" > 0;
            }
        }
    }
}

table 50710 "P7 Target"
{
    DataCaptionFields = "Code", Name;
    fields
    {
        field(1; "Code"; Code[20]) { }
        field(2; Base; Option) { OptionMembers = Static,Dynamic; }
        field(3; Name; Text[50]) { }
    }
    keys { key(PK; "Code") { } }

    trigger OnInsert()
    begin
        Base := xRec.Base;
    end;
}

page 50702 "P7 Target List"
{
    PageType = List;
    SourceTable = "P7 Target";

    layout
    {
        area(Content)
        {
            repeater(Lines)
            {
                field(Base; Rec.Base) { }
            }
        }
    }
}

codeunit 50711 "P7 UI Tests"
{
    Subtype = Test;

    var
        ModalSeen: Boolean;

    [Test]
    [HandlerFunctions('VisModalHandler')]
    procedure TrapThenRunModal()
    var
        TP: TestPage "P7 Vis Page";
    begin
        TP.Trap();
        Page.RunModal(Page::"P7 Vis Page");
        if not ModalSeen then
            Error('the ModalPageHandler did not run');
    end;

    [ModalPageHandler]
    procedure VisModalHandler(var VisPage: TestPage "P7 Vis Page")
    begin
        ModalSeen := true;
    end;

    [Test]
    procedure TrapNewestTakesPage()
    var
        Item: Record "P7 Item";
        Old: TestPage "P7 Page";
        New: TestPage "P7 Page";
    begin
        // a trap whose page never opened (an earlier test's) does not take the page
        Old.Trap();
        New.Trap();
        Old.Trap();
        New.Trap();
        Page.Run(Page::"P7 Page", Item);
        New.Close();
        Page.Run(Page::"P7 Page", Item);
        Old.Close();
    end;

    [Test]
    procedure LeaveTrap()
    var
        Stale: TestPage "P7 Page";
    begin
        // the TestPage is disposed when the method ends, and its trap with it
        Stale.Trap();
    end;

    [Test]
    procedure TrapAfterLeft()
    var
        Other: TestPage "P7 Vis Page";
    begin
        Other.Trap();
    end;
}

page 50703 "P7 Item List"
{
    PageType = List;
    SourceTable = "P7 Item";

    layout
    {
        area(Content)
        {
            repeater(Lines)
            {
                field("No."; Rec."No.") { }
            }
        }
    }

    trigger OnOpenPage()
    begin
        // as GenJnlManagement.OpenJnlBatch: a filter in group 0, group 2 left active
        Rec.FilterGroup(0);
        Rec.SetRange("No.", 'FG-A');
        Rec.FilterGroup(2);
    end;
}

page 50704 "P7 Var Page"
{
    PageType = Card;
    SourceTable = "P7 Item";

    layout
    {
        area(Content)
        {
            field(QtyText; QtyText)
            {
                trigger OnValidate()
                begin
                    Evaluate(Rec."Qty. Count", QtyText);
                end;
            }
        }
    }

    var
        QtyText: Text;
}

page 50705 "P7 Group List"
{
    PageType = List;
    SourceTable = "P7 Item";

    layout
    {
        area(Content)
        {
            repeater(Lines)
            {
                field("No."; Rec."No.") { }
                field(Pct; Rec.Pct) { }
                field(SeenCurrField; Rec.SeenCurrField) { }
            }
        }
    }

    trigger OnOpenPage()
    begin
        // a private filter group (as General Journal's simple mode, group 25) and the user's (0)
        Rec.FilterGroup := 25;
        Rec.SetRange(Pct, 7);
        Rec.FilterGroup := 0;
        Rec.SetRange(SeenCurrField, 3);
    end;
}

dotnet
{
    assembly("Microsoft.Dynamics.Nav.Ncl")
    {
        type("Microsoft.Dynamics.Nav.Runtime.LastError"; "LastError") { }
    }
    assembly("Microsoft.Dynamics.Nav.ClientExtensions")
    {
        type("Microsoft.Dynamics.Nav.Client.Hosts.OutlookCommand"; "OutlookCommand") { }
        type("Microsoft.Dynamics.Nav.Client.Hosts.OfficeHostType"; "OfficeHostType") { }
    }
    assembly("mscorlib")
    {
        type("System.String"; "P7NetString") { }
    }
    assembly("System")
    {
        type("System.Text.RegularExpressions.Regex"; "P7Regex") { }
    }
    assembly("Microsoft.Dynamics.Nav.NavUserAccount")
    {
        type("Microsoft.Dynamics.Nav.NavUserAccount.NavUserAccountHelper"; "NavUserAccountHelper") { }
    }
}

table 50711 "P7 Entry"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; "Target Code"; Code[20]) { TableRelation = "P7 Target"; }
    }
    keys { key(PK; "Entry No.") { } }
}

page 50706 "P7 Entries"
{
    PageType = List;
    SourceTable = "P7 Entry";
    DataCaptionFields = "Target Code";

    layout
    {
        area(Content)
        {
            repeater(Lines)
            {
                field("Entry No."; Rec."Entry No.") { }
            }
        }
    }
}

page 50707 "P7 Expr Caption"
{
    PageType = Card;
    SourceTable = "P7 Item";
    DataCaptionExpression = CapText;

    trigger OnOpenPage()
    begin
        CapText := 'Hello';
    end;

    var
        CapText: Text;
}

table 50712 "P7 Doc"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Descr; Text[50]) { }
    }
    keys { key(PK; "No.") { } }

    trigger OnInsert()
    begin
        if "No." = '' then
            "No." := 'DOC1';
    end;
}

table 50713 "P7 Doc Line"
{
    fields
    {
        field(1; "Doc No."; Code[20]) { }
        field(2; "Line No."; Integer) { }
        field(3; Qty; Decimal)
        {
            trigger OnValidate()
            var
                Doc: Record "P7 Doc";
            begin
                Doc.Get("Doc No.");
            end;
        }
    }
    keys { key(PK; "Doc No.", "Line No.") { } }
}

page 50708 "P7 Doc Lines"
{
    PageType = ListPart;
    SourceTable = "P7 Doc Line";
    AutoSplitKey = true;

    layout
    {
        area(Content)
        {
            repeater(Lines)
            {
                field(Qty; Rec.Qty) { }
            }
        }
    }
}

page 50709 "P7 Doc Card"
{
    PageType = Document;
    SourceTable = "P7 Doc";

    layout
    {
        area(Content)
        {
            field("No."; Rec."No.") { }
            field(Descr; Rec.Descr) { }
            part(Lines; "P7 Doc Lines")
            {
                SubPageLink = "Doc No." = field("No.");
            }
        }
    }
}

pageextension 50700 "P7 Page Ext" extends "P7 Page"
{
    var
        ExtFlag: Boolean;

    procedure SetExtFlag()
    begin
        ExtFlag := true;
    end;

    procedure GetExtFlag(): Boolean
    begin
        exit(ExtFlag);
    end;
}

codeunit 50701 "P7 Tests"
{
    procedure PropAssign(): Text
    var
        Lib: Codeunit "P7 Lib";
        Item: Record "P7 Item";
    begin
        Lib.SetX := 42;
        Item.SetFlag := true;
        exit(Format(Lib.GetX()) + '|' + Format(Item.Flag));
    end;

    procedure CurrFieldFromCode(): Text
    var
        Item: Record "P7 Item";
    begin
        Item.Validate(Amount, 5);
        exit(Format(Item.SeenCurrField) + '|' + Format(Item.SetCurrFieldAndRead()));
    end;

    procedure Dialogs(): Text
    var
        Window: Dialog;
    begin
        Window.Open('Processing #1####');
        Window.Update(1, 'x');
        Window.HideSubsequentDialogs(true);
        Window.Close();
        exit(Format(Dialog.Confirm('Go on?', true)));
    end;

    procedure OptionFormat(): Text
    var
        Opt: Option Alpha,Beta,"Gamma Delta";
        Opt2: Option Alpha,Beta,"Gamma Delta";
        Item: Record "P7 Item";
        Color: Enum "P7 Color";
        Ok1: Boolean;
        Ok2: Boolean;
    begin
        Opt := Opt::Beta;
        Ok1 := Evaluate(Opt2, 'Gamma Delta');
        Ok2 := Evaluate(Item.Kind, 'Special Kind');
        Color := Color::Green;
        exit(Format(Opt) + '|' + Format(Opt, 0, 9) + '|' + Format(Opt, 0, 2) + '|' + StrSubstNo('%1', Opt) + '|' +
            Format(Opt2) + Format(Ok1) + '|' + OptParam(2) + '|' + Format(Item.Kind) + Format(Ok2) + '|' +
            Format(Color) + '|' + Format(Color, 0, 9));
    end;

    local procedure OptParam(O: Option A,B,C): Text
    begin
        exit(Format(O));
    end;

    procedure Notifications(): Text
    var
        N: Notification;
        N2: Notification;
        N3: Notification;
        Added: Boolean;
        Missing: Boolean;
    begin
        N.Message('hi');
        N.SetData('k', 'v');
        N.Scope := NotificationScope::LocalScope;
        Added := N.AddAction('Do', Codeunit::"P7 Lib", 'ActionHandler');
        Missing := N.AddAction('Do', Codeunit::"P7 Lib", 'NoSuchMethod');
        N.Send();
        N2 := N;
        N2.Message('changed');
        exit(N.Message() + '|' + N.GetData('k') + '|' + Format(N.HasData('x')) + '|' + Format(IsNullGuid(N.Id())) + '|' +
            Format(Added) + Format(Missing) + '|' + Format(N3.Recall()));
    end;

    procedure SysFuncs(): Text
    begin
        RegisterTableConnection(TableConnectionType::CRM, 'TEST', '@@test@@');
        SetDefaultTableConnection(TableConnectionType::CRM, 'TEST');
        if not HasTableConnection(TableConnectionType::CRM, 'TEST') then
            exit('not registered');
        UnregisterTableConnection(TableConnectionType::CRM, 'TEST');
        UnregisterTableConnection(TableConnectionType::CRM, 'UNKNOWN');
        exit(Sid() + '|' + Format(HasTableConnection(TableConnectionType::CRM, 'TEST')) + '|' +
            GetDefaultTableConnection(TableConnectionType::CRM) + '|' + Format(EncryptionEnabled()) + '|' +
            Format(IsSessionActive(SessionId())) + Format(IsSessionActive(SessionId() + 1)));
    end;

    procedure EmptyRange(): Text
    var
        Item: Record "P7 Item";
    begin
        Item."No." := 'A';
        Item.Insert();
        Item."No." := 'B';
        Item.Insert();
        Item.SetRange("No.", 'B', 'A');
        exit(Format(Item.FindFirst()) + '|' + Format(Item.Count()));
    end;

    procedure Overloads(): Text
    var
        Lib: Codeunit "P7 Lib";
        Lib2: Codeunit "P7 Lib2";
    begin
        exit(Which(Lib2) + Which(Lib));
    end;

    local procedure Which(L: Codeunit "P7 Lib"): Text
    begin
        exit('lib');
    end;

    local procedure Which(L: Codeunit "P7 Lib2"): Text
    begin
        exit('lib2');
    end;

    procedure EventSender(): Text
    var
        Pub: Codeunit "P7 Publisher";
        Item: Record "P7 Item";
    begin
        Pub.Run(Item);
        exit(Format(Pub.WasCalled()));
    end;

    procedure Arrays(): Text
    var
        M: array[2, 3] of Integer;
    begin
        M[2] [3] := 7;
        M[1, 2] := 4;
        exit(Format(M[2, 3]) + Format(M[2] [3]) + Format(M[1] [2]));
    end;

    procedure SelectStrRange(): Text
    begin
        asserterror SelectStr(3, 'a,b');
        exit(SelectStr(2, 'a,b') + Format(GetLastErrorText() <> ''));
    end;

    procedure EvaluateFieldRef(): Text
    var
        RecRef: RecordRef;
        FldRef: FieldRef;
        I: Integer;
    begin
        RecRef.Open(Database::"P7 Item");
        FldRef := RecRef.Field(5);
        if not Evaluate(FldRef, 'Special Kind') then
            exit('evaluate failed');
        I := FldRef.Value;
        exit(Format(I));
    end;

    procedure ErrorInfos(): Text
    var
        EI: ErrorInfo;
        EI2: ErrorInfo;
        D: Dictionary of [Text, Text];
    begin
        EI := ErrorInfo.Create('boom', true);
        EI.CustomDimensions.Add('k', 'v');
        EI.Title := 'T';
        EI2 := EI;
        EI2.Message := 'other';
        D := EI.CustomDimensions;
        exit(EI.Message + '|' + Format(EI.Collectible) + '|' + EI.Title + '|' + D.Get('k') + '|' + Format(IsCollectingErrors()));
    end;

    [ErrorBehavior(ErrorBehavior::Collect)]
    procedure CollectTwo(): Text
    var
        EI: ErrorInfo;
        Errs: List of [ErrorInfo];
        Last: ErrorInfo;
    begin
        EI := ErrorInfo.Create('first', true);
        Error(EI);
        EI.Message := 'second';
        Error(EI);
        Errs := GetCollectedErrors(true);
        Last := Errs.Get(2);
        exit(Format(Errs.Count) + Format(HasCollectedErrors()) + Format(IsCollectingErrors()) + Last.Message);
    end;

    [ErrorBehavior(ErrorBehavior::Collect)]
    local procedure CollectAndEnd()
    var
        EI: ErrorInfo;
    begin
        EI := ErrorInfo.Create('collected one', true);
        Error(EI);
        EI := ErrorInfo.Create('not collectible', false);
        if false then
            Error(EI);
    end;

    procedure CollectRaisedAtEnd(): Text
    begin
        asserterror CollectAndEnd();
        exit(GetLastErrorText());
    end;

    procedure OptionArray(): Text
    var
        AF: array[2] of Option " ",SchPrice,UsagePrice;
    begin
        AF[1] := AF[1]::UsagePrice;
        exit(Format(AF[1] = AF[1]::UsagePrice) + Format(AF[1]) + Format(AF[2]::SchPrice));
    end;

    procedure PageExtGlobals(): Text
    var
        P: Page "P7 Page";
    begin
        P.SetExtFlag();
        exit(Format(P.GetExtFlag()));
    end;

    procedure EnumFieldRefOptions(): Text
    var
        RecRef: RecordRef;
        FldRef: FieldRef;
    begin
        RecRef.Open(Database::"P7 Item");
        FldRef := RecRef.Field(6);
        exit(FldRef.OptionMembers + '|' + FldRef.OptionCaption + '|' + SelectStr(2, FldRef.OptionCaption));
    end;

    procedure RecordLinks(): Text
    var
        A: Record "P7 Item";
        B: Record "P7 Item";
        RecRef: RecordRef;
        Id: Integer;
        Had: Boolean;
    begin
        A."No." := 'LA';
        A.Insert();
        B."No." := 'LB';
        B.Insert();
        Had := A.HasLinks();
        Id := A.AddLink('https://example.com', 'Ex');
        A.AddLink('https://example.org');
        B.CopyLinks(A);
        RecRef.GetTable(B);
        exit(Format(Had) + Format(A.HasLinks()) + Format(Id > 0) + Format(RecRef.HasLinks()) + '|' +
            DeleteAndCount(A, Id) + '|' + DeleteAllAndCount(B));
    end;

    local procedure DeleteAndCount(var A: Record "P7 Item"; Id: Integer): Text
    var
        RecordLink: Record "Record Link";
    begin
        A.DeleteLink(Id);
        RecordLink.SetRange("Record ID", A.RecordId);
        exit(Format(RecordLink.Count));
    end;

    local procedure DeleteAllAndCount(var B: Record "P7 Item"): Text
    var
        RecordLink: Record "Record Link";
    begin
        RecordLink.SetRange("Record ID", B.RecordId);
        B.DeleteLinks();
        exit(Format(RecordLink.Count) + Format(B.HasLinks()));
    end;

    procedure NumberPictures(): Text
    var
        Month: Integer;
        D: Decimal;
    begin
        Month := 8;
        D := -1.5;
        exit(Format(Month, 0, '<Integer,2><Filler Character,0>') + '|' + Format(Month, 0, '<Integer,4>') + '|' +
            Format(123, 0, '<Integer,2>') + '|' + Format(1234567, 0, '<Integer Thousand>') + '|' +
            Format(D, 0, '<Sign><Integer><Decimals,3>') + '|' + Format(1.234, 0, '<Precision,2:2><Standard Format,0>') + '|' +
            Format(7, 0, '<Integer><Decimals,3>') + '|' + Format(2015, 0, '<Integer>') + '-' + Format(3, 0, '<Integer,2><Filler Character,0>'));
    end;

    procedure JsonSecrets(): Text
    var
        J: JsonObject;
        H: JsonObject;
        Tok: JsonToken;
        Secrets: Dictionary of [Text, Text];
        R: Text;
        R2: Text;
    begin
        H.Add('Authorization', 'x');
        J.Add('Header', H);
        J.Add('a', 1);
        J.WriteWithSecretsTo('$.Header.Authorization', 'Bearer 123', R);
        Secrets.Add('$.a', 's');
        J.WriteWithSecretsTo(Secrets, R2);
        J.SelectToken('$.Header.Authorization', Tok);
        exit(Format(R.Contains('"Authorization":"Bearer 123"')) + Format(R2.Contains('"a":"s"')) + Tok.AsValue().AsText());
    end;

    procedure ListOverloads(): Text
    var
        Ints: List of [Integer];
    begin
        GetIds(Ints);
        exit(Format(Ints.Count) + Format(Ints.Get(2)));
    end;

    local procedure GetIds(var Ids: List of [Integer])
    var
        Texts: List of [Text];
        T: Text;
        I: Integer;
    begin
        GetIds(Texts);
        foreach T in Texts do begin
            Evaluate(I, T);
            Ids.Add(I);
        end;
    end;

    local procedure GetIds(var Ids: List of [Text])
    begin
        Ids.Add('4');
        Ids.Add('7');
    end;

    procedure PictureAbbreviations(): Text
    var
        DT: DateTime;
        T: Text;
    begin
        DT := CreateDateTime(20240305D, 101112T);
        T := Format(DT, 0, '<Hours24,2>:<Minutes,2>:<Seconds,2><Second dec.>');
        exit(Format(7, 0, '<Integer,4><Filler Char,0>') + '|' + Format(1.5, 0, '<Prec,2:2><Standard Format,0>') + '|' +
            Format(12, 0, '<I,3><F,0>') + '|' + CopyStr(T, 1, 8) + Format(StrLen(T) < 14));
    end;

    procedure FileTransfer(): Text
    var
        InStr: InStream;
        ToFile: Text;
        FromFile: Text;
        Ok1: Boolean;
        Ok2: Boolean;
        Ok3: Boolean;
        Ok4: Boolean;
    begin
        ToFile := 'report.txt';
        Ok1 := DownloadFromStream(InStr, 'Export', '', 'Text files (*.txt)|*.txt', ToFile);
        Ok2 := File.Download('server.txt', 'Export', '', '', ToFile);
        Ok3 := UploadIntoStream('Import', '', '', FromFile, InStr);
        Ok4 := File.UploadIntoStream('Import', '', '', FromFile, InStr);
        exit(Format(Ok1) + Format(Ok2) + ToFile + Format(Ok3) + Format(Ok4) + FromFile);
    end;

    procedure InterfaceDefaults(): Text
    var
        G: Interface "P7 Greeter";
        Impl: Codeunit "P7 Greeter Impl";
    begin
        G := Impl;
        exit(G.Name() + '|' + G.Greet('x'));
    end;

    procedure TestFieldCodes(): Text
    var
        Item: Record "P7 Item";
        C1: Text;
    begin
        asserterror Item.TestField(Amount);
        C1 := GetLastErrorCode();
        Item.Amount := 1;
        asserterror Item.TestField(Amount, 2);
        exit(C1 + '|' + GetLastErrorCode());
    end;

    procedure DateFormulas(): Text
    var
        F: DateFormula;
        G: DateFormula;
        Ok: Boolean;
    begin
        Evaluate(F, Format('<+%1D>', 30));
        Evaluate(G, '<1M-D5>');
        Ok := Evaluate(G, 'TODAY');
        exit(Format(F) + '|' + Format(F, 0, 2) + '|' + Format(CalcDate(F, 20261107D), 0, 9) + '|' + Format(G) + '|' + Format(Ok));
    end;

    procedure CollectionErrors(): Text
    var
        D: Dictionary of [Text, Integer];
        L: List of [Integer];
        E: Text;
        i: Integer;
    begin
        D.Add('a', 1);
        asserterror i := D.Get('b');
        E := GetLastErrorText();
        asserterror D.Add('a', 2);
        E += '|' + GetLastErrorText();
        L.Add(1);
        asserterror i := L.Get(2);
        exit(E + '|' + GetLastErrorText());
    end;

    procedure BindLifetime(): Text
    var
        Pub: Codeunit "P7 Bind Pub";
        Sub: Codeunit "P7 Manual Sub";
        Sub2: Codeunit "P7 Manual Sub";
        Sub3: Codeunit "P7 Manual Sub";
        R: Text;
    begin
        BindLocal();
        R := Format(Pub.Ping());
        BindSubscription(Sub);
        R += Format(Pub.Ping());
        Sub2 := Sub;
        asserterror BindSubscription(Sub2);
        Clear(Sub);
        Clear(Sub2);
        R += Format(Pub.Ping());
        BindAndClear();
        R += Format(Pub.Ping());
        BindSubscription(Sub3);
        UnbindSubscription(Sub3);
        R += Format(Pub.Ping());
        exit(R);
    end;

    procedure ClearThenRebind(): Text
    var
        Pub: Codeunit "P7 Bind Pub";
        Sub: Codeunit "P7 Manual Sub";
        Other: Codeunit "P7 Manual Sub";
        R: Text;
    begin
        BindSubscription(Sub);
        Clear(Sub);
        BindSubscription(Sub);
        R := Format(Pub.Ping());
        Clear(Sub);
        BindSubscription(Other);
        R += Format(Pub.Ping());
        exit(R);
    end;

    local procedure BindAndClear()
    var
        L: Codeunit "P7 Manual Sub";
    begin
        BindSubscription(L);
        Clear(L);
    end;

    local procedure BindLocal()
    var
        L: Codeunit "P7 Manual Sub";
    begin
        BindSubscription(L);
    end;

    procedure DateFormulaFields(): Text
    var
        Item: Record "P7 Item";
        Ok1: Boolean;
        Ok2: Boolean;
    begin
        Ok1 := Evaluate(Item.DueCalc, '<Y>');
        Ok2 := Evaluate(Item.DueCalc, '<1M+CM>');
        asserterror Evaluate(Item.DueCalc, 'TODAY');
        exit(Format(Ok1) + Format(Ok2) + Format(Item.DueCalc) + '|' + Format(GetLastErrorText() <> '') + ClearedFormula(Item));
    end;

    local procedure ClearedFormula(var Item: Record "P7 Item"): Text
    begin
        Clear(Item.DueCalc);
        exit('|' + Format(Item.DueCalc) + '.');
    end;

    procedure CollectionTraps(): Text
    var
        L: List of [Integer];
        D: Dictionary of [Text, Integer];
        R: Text;
        V: Integer;
    begin
        L.Add(1);
        L.Add(2);
        R := Format(L.Set(3, 9)) + Format(L.RemoveAt(0)) + Format(L.Insert(4, 1)) + Format(L.Get(5, V));
        asserterror L.Set(3, 9);
        R += '|' + GetLastErrorText();
        asserterror L.RemoveAt(3);
        R += '|' + Format(GetLastErrorText() <> '');
        R += '|' + Format(D.Add('a', 1)) + Format(D.Add('a', 2));
        asserterror D.Get('b', V);
        R += '|' + GetLastErrorText();
        L.Set(2, 5);
        L.Insert(3, 7);
        exit(R + '|' + Format(L.Get(2)) + Format(L.Get(3)) + Format(L.Count()));
    end;

    procedure FindDirections(): Text
    var
        T: Record "P7 Item" temporary;
        R: Text;
    begin
        T."No." := 'A1';
        T.Insert();
        T."No." := 'A3';
        T.Insert();
        T."No." := 'A5';
        T.Insert();
        T."No." := 'A2';
        R := Format(T.Find('=<')) + T."No.";
        T."No." := 'A2';
        R += Format(T.Find('=>')) + T."No.";
        T."No." := 'A3';
        R += Format(T.Find('=<')) + T."No.";
        T."No." := 'A3';
        R += Format(T.Find('<>')) + T."No.";
        T."No." := 'A3';
        R += Format(T.Find('><')) + T."No.";
        T."No." := 'A0';
        R += Format(T.Find('=<')) + T."No.";
        T."No." := 'A0';
        R += Format(T.Find('=<>')) + T."No.";
        T.SetFilter("No.", '<>A1');
        T."No." := 'A2';
        R += Format(T.Find('=<')) + T."No.";
        exit(R);
    end;

    procedure TaskSchedulerRuns(): Text
    var
        Item: Record "P7 Item";
        Failed: Record "P7 Item";
        Id1: Guid;
        Id2: Guid;
        Id3: Guid;
        R: Text;
    begin
        Item."No." := 'T1';
        Item.Insert();
        Id1 := TaskScheduler.CreateTask(Codeunit::"P7 Task", 0, true, CompanyName(), 0DT, Item.RecordId);
        Id2 := TaskScheduler.CreateTask(Codeunit::"P7 Task", 0, false, CompanyName(), 0DT, Item.RecordId);
        Id3 := TaskScheduler.CreateTask(Codeunit::"P7 Task", 0, true, CompanyName(), 0DT, Item.RecordId);
        R := Format(TaskScheduler.TaskExists(Id1)) + Format(TaskScheduler.CancelTask(Id3)) + Format(TaskScheduler.TaskExists(Id3));
        Commit();
        Sleep(0);
        Item.Get('T1');
        R += '|' + Format(Item.Amount) + Format(TaskScheduler.TaskExists(Id1)) + Format(TaskScheduler.TaskExists(Id2));
        TaskScheduler.SetTaskReady(Id2);
        Sleep(0);
        Item.Get('T1');
        R += '|' + Format(Item.Amount);
        Item.Flag := true;
        Item.Modify();
        TaskScheduler.CreateTask(Codeunit::"P7 Task", Codeunit::"P7 Task Failure", true, CompanyName(), 0DT, Item.RecordId);
        Sleep(0);
        Item.Get('T1');
        R += '|' + Format(Item.Amount) + Format(Failed.Get('FAILED')) + Format(Failed.Amount);
        exit(R);
    end;

    procedure RecScopes(): Text
    var
        Item: Record "P7 Item";
    begin
        exit(Format(Item.LineType::"Charge (Item)") + '|' + Item.ScopeFromRec());
    end;

    procedure VariantDateParam(): Text
    var
        V: Variant;
        D: Date;
    begin
        V := CreateDateTime(20280127D, 120000T);
        D := V;
        exit(DateParam(V) + '|' + Format(D, 0, 9));
    end;

    local procedure DateParam(D: Date): Text[10]
    begin
        exit(Format(D, 0, 9));
    end;

    procedure VisibleWithDottedField(): Text
    var
        Item: Record "P7 Item";
        TP: TestPage "P7 Vis Page";
        R: Text;
    begin
        Item."No." := 'V1';
        Item.Insert();
        TP.OpenView();
        TP.GoToRecord(Item);
        R := Format(TP.Flag.Visible());
        TP.Close();
        Item."Qty. Count" := 2;
        Item.Modify();
        TP.OpenView();
        TP.GoToRecord(Item);
        R += Format(TP.Flag.Visible());
        TP.Close();
        exit(R);
    end;

    procedure PageInsertXRec(): Text
    var
        T: Record "P7 Target";
        TP: TestPage "P7 Target List";
        R: Text;
    begin
        TP.OpenEdit();
        TP.Base.SetValue(T.Base::Dynamic);
        R := TP.Base.Value();
        TP.Close();
        T.FindFirst();
        exit(R + '|' + Format(T.Base));
    end;

    procedure TestPageFilterGroup(): Text
    var
        Item: Record "P7 Item";
        TP: TestPage "P7 Item List";
        R: Text;
    begin
        Item."No." := 'FG-A';
        Item.Insert();
        Item."No." := 'FG-B';
        Item.Insert();
        TP.OpenView();
        R := TP."No.".Value();
        TP.Filter.SetFilter("No.", 'FG-B');
        R += '|' + TP."No.".Value() + '|' + TP.Filter.GetFilter("No.");
        TP.Close();
        exit(R);
    end;

    procedure ArrayRows(): Text
    var
        V: array[3, 2] of Decimal;
    begin
        FillRow(V[2]);
        V[3, 1] := 7;
        V[3, 2] := 1;
        exit(Format(V[2, 1]) + ',' + Format(V[2, 2]) + ',' + Format(SumRow(V[3])) + ',' + Format(V[1, 1]) + ',' + Format(V[3, 2]));
    end;

    local procedure FillRow(var Row: array[2] of Decimal)
    begin
        Row[1] := 5;
        Row[2] := 6;
    end;

    local procedure SumRow(Row: array[2] of Decimal): Decimal
    begin
        Row[2] := 100;
        exit(Row[1] + Row[2]);
    end;

    procedure FieldTypeNames(): Text
    var
        Item: Record "P7 Item";
        RecRef: RecordRef;
        FRef: FieldRef;
    begin
        RecRef.GetTable(Item);
        FRef := RecRef.Field(Item.FieldNo(Flag));
        exit(Format(FRef.Type) + ',' + StrSubstNo('%1', FRef.Type) + ',' + Format(FieldType::Date) + ',' + Format(FRef.Type = FieldType::Boolean));
    end;

    procedure DivideByZeroCode(): Text
    var
        Zero: Integer;
        X: Decimal;
    begin
        asserterror X := 1 / Zero;
        exit(GetLastErrorCode() + '|' + GetLastErrorText());
    end;

    procedure DateTimeStdFormats(): Text
    var
        DT: DateTime;
        DT0: DateTime;
    begin
        DT0 := CreateDateTime(20280127D, 131415T);
        DT := DT0 + 120;
        exit(Format(DT, 0, 0) + '|' + Format(DT, 0, 1) + '|' + Format(DT0, 0, 1) + '|' + Format(DT, 0, 3));
    end;

    procedure BindTwiceTrapped(): Text
    var
        Sub: Codeunit "P7 Manual Sub";
        R: Text;
    begin
        BindSubscription(Sub);
        R := Format(BindSubscription(Sub));
        asserterror BindSubscription(Sub);
        R += '|' + GetLastErrorText();
        UnbindSubscription(Sub);
        exit(R);
    end;

    procedure PageVarSavesRecord(): Text
    var
        Item: Record "P7 Item";
        TP: TestPage "P7 Var Page";
    begin
        Item."No." := 'VP1';
        Item.Insert();
        TP.OpenEdit();
        TP.GoToRecord(Item);
        TP.QtyText.SetValue('7');
        TP.Close();
        Item.Get('VP1');
        exit(Format(Item."Qty. Count"));
    end;

    procedure NewRecordFromFilters(): Text
    var
        TP: TestPage "P7 Group List";
        R: Text;
    begin
        TP.OpenNew();
        R := Format(TP.Pct.Value()) + '|' + Format(TP.SeenCurrField.Value());
        TP.Close();
        exit(R);
    end;

    procedure RethrowLastError(): Text
    var
        LastError: DotNet LastError;
        Item: Record "P7 Item";
    begin
        asserterror Item.Get('NOSUCHITEM');
        asserterror LastError.Rethrow();
        exit(GetLastErrorCode() + '|' + CopyStr(GetLastErrorText(), 1, 20));
    end;

    procedure DataCaptions(): Text
    var
        T: Record "P7 Target";
        TP: TestPage "P7 Entries";
        TP2: TestPage "P7 Expr Caption";
        R: Text;
    begin
        T."Code" := 'TGT1';
        T.Name := 'Target One';
        T.Insert();
        TP.OpenView();
        TP.Filter.SetFilter("Target Code", 'TGT1');
        R := TP.Caption();
        TP.Close();
        TP2.OpenView();
        R += '|' + TP2.Caption();
        TP2.Close();
        exit(R);
    end;

    procedure DateFormulaGerman(): Text
    var
        DF: DateFormula;
        R: Text;
        Prev: Integer;
    begin
        Evaluate(DF, '<1Y+CM-1D>');
        Prev := GlobalLanguage();
        GlobalLanguage(1031);
        R := Format(DF) + '|' + Format(DF, 0, 2) + '|' + Format(DF, 0, 9);
        // the German text evaluates back
        Evaluate(DF, Format(DF));
        R += '|' + Format(DF, 0, 9);
        GlobalLanguage(Prev);
        exit(R + '|' + Format(DF));
    end;

    procedure PartSavesHeader(): Text
    var
        TP: TestPage "P7 Doc Card";
        L: Record "P7 Doc Line";
    begin
        TP.OpenNew();
        TP.Lines.Qty.SetValue(5);
        TP.Close();
        L.FindFirst();
        exit(L."Doc No." + '|' + Format(L.Qty));
    end;

    procedure ClearVarArray(): Text
    var
        V: array[16] of Decimal;
        i: Integer;
    begin
        for i := 1 to 16 do
            V[i] := i;
        ClearSmall(V);
        exit(Format(ArrayLen(V)) + '|' + Format(V[16]) + '|' + Format(V[1]));
    end;

    local procedure ClearSmall(var A: array[14] of Decimal)
    begin
        Clear(A);
        A[1] := 7;
    end;

    procedure ClearTempRecordArray(): Text
    var
        TempItems: array[2] of Record "P7 Item" temporary;
    begin
        // each element is cleared as Clear(Rec) does (BC ElementFactory.ClearElement ->
        // NavComplexValue.Clear): values and filters go, the temporary table's rows stay
        TempItems[1]."No." := 'TA1';
        TempItems[1].Insert();
        TempItems[1].SetRange("No.", 'TA1');
        TempItems[1].Amount := 5;
        Clear(TempItems);
        exit(Format(TempItems[1].Count()) + '|' + TempItems[1].GetFilters() + '|' + Format(TempItems[1].Amount) + '|' + Format(ArrayLen(TempItems)));
    end;

    procedure EffectivePermissionText(): Text
    var
        UserAccountHelper: DotNet NavUserAccountHelper;
        R: Integer;
        T: Text;
    begin
        T := UserAccountHelper.GetEffectivePermissionForObject(UserSecurityId(), CompanyName(), 1, 18);
        Evaluate(R, SelectStr(1, T));
        exit(T + '|' + Format(R));
    end;

    procedure PageControlFields(): Text
    var
        PageControlField: Record "Page Control Field";
        T: Text;
    begin
        // as Workflow Webhook Subscription.AddEventConditions / Edit in Excel: the page's
        // data-bound controls, in definition order (groups and parts are not rows)
        PageControlField.SetRange(PageNo, Page::"P7 Control Fields");
        PageControlField.SetCurrentKey(Sequence);
        if PageControlField.FindSet() then
            repeat
                T += StrSubstNo('%1:%2:%3:%4:%5:%6:%7:%8;', PageControlField.Sequence, PageControlField.ControlName, PageControlField.TableNo,
                    PageControlField.FieldNo, PageControlField.SourceExpression, PageControlField.OptionString, PageControlField.Editable, PageControlField.Visible);
            until PageControlField.Next() = 0;
        // as API Webhook Notification Send.HasLastModifiedDateTimeField
        PageControlField.SetRange(ControlName, 'Item No.');
        PageControlField.SetFilter(Visible, '%1', '@TRUE');
        exit(T + Format(PageControlField.Count()));
    end;

    procedure CurrPageClose(): Text
    var
        CloseDlg: TestPage "P7 Close Dlg";
        T: Text;
    begin
        // as Outlook Add-in Commands' CustomerDrillDownCreate: the drilldown closes the dialog
        CloseDlg.OpenView();
        asserterror CloseDlg.CloseCtl.DrillDown();
        T := GetLastErrorText();
        // an action that closes the page: the TestPage is closed afterwards
        CloseDlg.OpenView();
        CloseDlg.CloseAct.Invoke();
        asserterror T := T + CloseDlg.CloseCtl.Value();
        exit(T + '|' + GetLastErrorText());
    end;

    procedure StreamUrlAccess(): Text
    var
        Holder: Record "Tenant Media" temporary;
        Media: Record "Tenant Media";
        OutStr: OutStream;
        InStr: InStream;
        Id1: Guid;
        Id2: Guid;
        Url1: Text;
        Url2: Text;
    begin
        // as Mail Management.ImageBase64ToUrl: the stream gets a URL until it expires
        Holder.Content.CreateOutStream(OutStr);
        OutStr.WriteText('png bytes');
        Holder.Content.CreateInStream(InStr);
        Id1 := ImportStreamWithUrlAccess(InStr, 'a.png', 20160);
        Id2 := ImportStreamWithUrlAccess(InStr, 'b.png');
        Url1 := GetDocumentUrl(Id1);
        Url2 := GetDocumentUrl(Id2);
        Media.Get(Id1);
        exit(Format((CopyStr(Url1, 1, 4) = 'http') and (StrPos(Url1, '/OpenDynamicFileHandler.axd?token=') > 0)) + '|' + Format(Url1 <> Url2) + '|' +
            Format(Media."Security Token" <> '') + '|' + Format(Media."Expiration Date" > CurrentDateTime() + 13 * 86400000) + '|' + GetDocumentUrl(CreateGuid()));
    end;

    procedure ForEachNetToCode(): Text
    var
        Parts: DotNet P7NetString;
        Sep: DotNet P7NetString;
        DocNo: Code[5];
        T: Text;
    begin
        // as Office Document Handler.RedirectToDocument: an element longer than the loop
        // variable is kept (BC NavCode.CreateFromObject)
        Parts := 'sales order# so1|ab';
        Sep := '|';
        foreach DocNo in Parts.Split(Sep.ToCharArray()) do
            T += DocNo + ';';
        exit(T);
    end;

    procedure OtherRecControl(): Text
    var
        TP: TestPage "P7 Other Rec";
    begin
        // as Payment Application Review's MatchConfidence (BankPmtApplRule."Match Confidence")
        TP.OpenView();
        exit(TP.OtherKind.Value());
    end;

    procedure PageVarNavigatesOrLooksUp(): Text
    var
        A: Record "P7 Item";
        B: Record "P7 Item";
        TP: TestPage "P7 Nav Page";
        T: Text;
    begin
        // as Manager Time Sheet's CurrTimeSheetNo: a validate that moves Rec saves nothing
        A."No." := 'NV-A';
        A.Insert();
        B."No." := 'NV-B';
        B.Insert();
        TP.OpenEdit();
        TP.GoToRecord(A);
        TP.GoNo.SetValue('NV-B');
        B.Get('NV-B');
        T := Format(B.Flag);
        // as G/L Account Card's subcategory lookup: the changed record is saved
        TP.GoNo.Lookup();
        B.Get('NV-B');
        exit(T + '|' + Format(B.Amount) + Format(B.Flag));
    end;

    procedure QuotedOptionFilter(): Text
    var
        Prio: Record "P7 Prio";
        TP: TestPage "P7 Prio List";
        N: Integer;
    begin
        // as Contact Card Subform's view: filter(<> "Very Low (Hidden)")
        Prio."Code" := 'A';
        Prio.Prio := Prio.Prio::Normal;
        Prio.Insert();
        Prio."Code" := 'B';
        Prio.Prio := Prio.Prio::"Very Low (Hidden)";
        Prio.Insert();
        TP.OpenView();
        if TP.First() then
            repeat
                N += 1;
            until not TP.Next();
        exit(Format(N));
    end;

    procedure RegexStaticViaInstance(): Text
    var
        Rx: DotNet P7Regex;
    begin
        // as Office Addin Initiate Tasks: static overloads through a constructed Regex variable
        Rx := Rx.Regex('^abc$');
        exit(Format(Rx.IsMatch('http://x', 'https?://.*')) + Format(Rx.IsMatch('abc')) + Rx.Replace('aXb', 'X', '-') + Rx.Replace('abc', '<$0>'));
    end;

    procedure RecordIdFieldFromFilter(): Text
    var
        Item: Record "P7 Item";
        Link: Record "P7 RecLink";
    begin
        // as Approval Comment Line.OnInsert: Evaluate("Record ID to Approve", GetFilter(...))
        Item."No." := 'RID1';
        Link.SetRange("Rec ID", Item.RecordId);
        Evaluate(Link."Rec ID", Link.GetFilter("Rec ID"));
        exit(Format(Link."Rec ID" = Item.RecordId) + '|' + Format(Link."Rec ID"));
    end;

    procedure RecRefRecordIdMember(): Text
    var
        Buf: Record "P7 RidBuf";
        Link: Record "P7 RecLink";
        RecRef: RecordRef;
        V: Variant;
    begin
        // as Workflow Engine UT: RecRef.RecordId on a table with a field named RecordID
        Buf.ID := 7;
        Buf.Insert();
        RecRef.GetTable(Buf);
        Link."Rec ID" := RecRef.RecordId;
        V := Buf;
        RecRef.GetTable(V);
        exit(Format(Link."Rec ID") + '|' + Format(RecRef.RecordId()));
    end;

    procedure ClearVariantKeepsRecord(): Text
    var
        Item: Record "P7 Item";
        Got: Record "P7 Item";
        Store: array[3] of Variant;
        V: Variant;
        Out: Variant;
    begin
        // as Workflow Record Management.RestoreRecord: Variant := VarArray[Index]; Clear(VarArray[Index])
        Item."No." := 'CV1';
        Store[1] := Item;
        Out := Store[1];
        Clear(Store[1]);
        Got := Out;
        V := Item;
        Clear(V);
        Store[2] := Item;
        Clear(Store);
        exit(Got."No." + '|' + Format(Store[1].IsRecord) + '|' + Format(V.IsRecord) + '|' + Format(Store[2].IsRecord) + '|' + Item."No.");
    end;

    procedure TryEventPublisher(): Text
    var
        Pub: Codeunit "S4 Try Pub";
        Sub: Codeunit "S4 Try Sub";
        R: Text;
    begin
        // as Workflow Webhook Notification.OnPostNotificationRequest: a [TryFunction] publisher is
        // true when no subscriber raised an error, false when one did (its [TryFunction] does not
        // swallow the error); with no bound subscriber it is true
        R := Format(Pub.Send());
        BindSubscription(Sub);
        Sub.SetFail(true);
        R += '|' + Format(Pub.Send()) + ':' + GetLastErrorText();
        Sub.SetFail(false);
        R += '|' + Format(Pub.Send());
        UnbindSubscription(Sub);
        exit(R);
    end;

    procedure OutlookCommands(): Text
    var
        OutlookCommand: DotNet OutlookCommand;
        OfficeHostType: DotNet OfficeHostType;
        Cmd: Text;
    begin
        Cmd := '';
        case Cmd of
            OutlookCommand.NewPurchaseCreditMemo, OutlookCommand.NewPurchaseInvoice:
                exit('vendor');
        end;
        exit(OutlookCommand.NewSalesQuote + '|' + OutlookCommand.NewPurchaseOrder + '|' + OfficeHostType.OutlookHyperlink);
    end;

    procedure TextStatics(): Text
    begin
        exit(Text.UpperCase('abc') + Text.CopyStr('hello', 2, 3));
    end;

    procedure BareProps(): Text
    var
        Item: Record "P7 Item";
    begin
        exit(Item.BareProperties());
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("p7-gaps-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
    let ci = cat.code.iter().position(|c| c.name == "P7 Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

/// TestPage traps: the most recent trap of a page takes it (BC NavTestExecution.FindTrap), a
/// TestPage is trapped once (AddTrap)
#[test]
fn trap_newest_first() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let ci = cat.code.iter().position(|c| c.name == "P7 UI Tests").unwrap() as u32;
    let pi = cat.code[ci as usize].find("TrapNewestTakesPage")[0];
    let mut s = Session::new(&rt, "CRONUS");
    let inst = Instance::new(ci);
    s.test = Some(TestCtx::new(cat, inst.clone(), pi).unwrap());
    match s.invoke(ProcRef { code: ci, proc: pi }, vec![], Some(inst), None, None) {
        Ok(_) => {}
        Err(Flow::Err(e)) => panic!("{}", e.msg),
        Err(_) => panic!("unexpected flow"),
    }
    assert!(s.traps.is_empty(), "traps left: {}", s.traps.len());
    // a trap of a TestPage whose variable is gone (an earlier test's local) is removed
    for name in ["LeaveTrap", "TrapAfterLeft"] {
        let pi = cat.code[ci as usize].find(name)[0];
        let inst = Instance::new(ci);
        s.test = Some(TestCtx::new(cat, inst.clone(), pi).unwrap());
        if let Err(Flow::Err(e)) = s.invoke(ProcRef { code: ci, proc: pi }, vec![], Some(inst), None, None) {
            panic!("{name}: {}", e.msg);
        }
    }
    let pages: Vec<i64> = s.traps.iter().map(|t| t.borrow().page).collect();
    assert_eq!(pages, vec![50701], "the stale P7 Page trap is gone, the last one stays until the next check");
}

/// RunModal with both a trapped TestPage and a ModalPageHandler for the page: the handler runs
#[test]
fn modal_handler_before_trap() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let ci = cat.code.iter().position(|c| c.name == "P7 UI Tests").unwrap() as u32;
    let pi = cat.code[ci as usize].find("TrapThenRunModal")[0];
    let mut s = Session::new(&rt, "CRONUS");
    let inst = Instance::new(ci);
    s.test = Some(TestCtx::new(cat, inst.clone(), pi).unwrap());
    match s.invoke(ProcRef { code: ci, proc: pi }, vec![], Some(inst), None, None) {
        Ok(_) => {}
        Err(Flow::Err(e)) => panic!("{}", e.msg),
        Err(_) => panic!("unexpected flow"),
    }
    let missing = s.test.as_ref().unwrap().not_executed();
    assert!(missing.is_empty(), "handlers not executed {missing:?}");
}

#[test]
fn interpreter_gaps() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    assert_eq!(call(&mut s, cat, "PropAssign"), "42|Yes");
    // Validate from code leaves CurrFieldNo at 0; AL can assign it
    assert_eq!(call(&mut s, cat, "CurrFieldFromCode"), "0|2");
    assert_eq!(call(&mut s, cat, "Dialogs"), "Yes");
    assert_eq!(
        call(&mut s, cat, "OptionFormat"),
        "Beta|1|1|Beta|Gamma DeltaYes|C|Special KindYes|Green Caption|5"
    );
    assert_eq!(call(&mut s, cat, "Notifications"), "hi|v|No|No|YesNo|No");
    assert_eq!(s.sys.notifications.len(), 1);
    assert_eq!(call(&mut s, cat, "SysFuncs"), "|No||No|YesNo");
    assert_eq!(call(&mut s, cat, "EmptyRange"), "No|0");
    assert_eq!(call(&mut s, cat, "BareProps"), "Yes");
    assert_eq!(call(&mut s, cat, "Overloads"), "lib2lib");
    assert_eq!(call(&mut s, cat, "EventSender"), "Yes");
    assert_eq!(call(&mut s, cat, "Arrays"), "774");
    assert_eq!(call(&mut s, cat, "SelectStrRange"), "bYes");
    assert_eq!(call(&mut s, cat, "EvaluateFieldRef"), "1");
    assert_eq!(call(&mut s, cat, "TextStatics"), "ABCell");
    // DotNet OutlookCommand: the command names (a blank command is no vendor command)
    assert_eq!(call(&mut s, cat, "OutlookCommands"), "New-SalesQuote|New-PurchaseOrder|Outlook-Hyperlink");
    // a [TryFunction] event publisher returns whether its subscribers raised an error
    assert_eq!(call(&mut s, cat, "TryEventPublisher"), "Yes|No:post failed|Yes");
    // Clear(Variant) empties the Variant and keeps the record it held
    assert_eq!(call(&mut s, cat, "ClearVariantKeepsRecord"), "CV1|No|No|No|CV1");
    // RecRef.RecordId is the RecordRef member even when the table has a field "RecordID"
    assert_eq!(call(&mut s, cat, "RecRefRecordIdMember"), "P7 RidBuf: 7|P7 RidBuf: 7");
    // Evaluate(RecordID field, GetFilter(RecordID field)): the formatted RecordID
    assert_eq!(call(&mut s, cat, "RecordIdFieldFromFilter"), "Yes|P7 Item: RID1");
    // Regex.IsMatch(Input, Pattern) / Replace(Input, Pattern, Replacement) through an instance
    assert_eq!(call(&mut s, cat, "RegexStaticViaInstance"), "YesYesa-b<abc>");
    // filter(<> "Very Low (Hidden)"): the quoted option name is one value
    assert_eq!(call(&mut s, cat, "QuotedOptionFilter"), "1");
    // page-variable triggers: navigation is no change, a lookup that changed Rec saves it
    assert_eq!(call(&mut s, cat, "PageVarNavigatesOrLooksUp"), "No|42Yes");
    // a control on another record's Option field shows the option caption
    assert_eq!(call(&mut s, cat, "OtherRecControl"), "Special Kind");
    // foreach over a DotNet enumeration: no declared-length check on the loop variable
    assert_eq!(call(&mut s, cat, "ForEachNetToCode"), "SALES ORDER# SO1;AB;");
    // ImportStreamWithUrlAccess + GetDocumentUrl: a Tenant Media row with a token and an expiry
    assert_eq!(call(&mut s, cat, "StreamUrlAccess"), "Yes|Yes|Yes|Yes|");
    // CurrPage.Close() closes the TestPage; a DrillDown that closed it raises BC's error
    assert_eq!(call(&mut s, cat, "CurrPageClose"), "The TestPage is not open.|The TestPage is not open.");
    // "Page Control Field": the page's field controls from the page metadata
    assert_eq!(
        call(&mut s, cat, "PageControlFields"),
        "0:Item No.:50700:1:No.:::true;1:KindCtl:50700:5:Kind:Normal,Special:false:true;2:Note:50700:0:NoteTxt:::Rec.Flag;1"
    );
    // NavUserAccountHelper.GetEffectivePermissionForObject: permissions as option ordinals
    assert_eq!(call(&mut s, cat, "EffectivePermissionText"), "1,1,1,1,1|1");
    // Clear on a var array parameter clears the caller's array in place, keeping its length
    assert_eq!(call(&mut s, cat, "ClearVarArray"), "16|0|7");
    // Clear on an array of temporary records: each record cleared, the temporary rows stay
    assert_eq!(call(&mut s, cat, "ClearTempRecordArray"), "1||0|2");
    // a value in a new document's lines saves the header first; the line takes its No.
    assert_eq!(call(&mut s, cat, "PartSavesHeader"), "DOC1|5");
    // Format(DateFormula) in a German session: German unit letters (formats 0/1), invariant (2/9)
    assert_eq!(call(&mut s, cat, "DateFormulaGerman"), "1J+LM-1T|<1Y+CM-1D>|1Y+CM-1D|1Y+CM-1D|1Y+CM-1D");
    // TestPage.Caption: caption and data caption (related record's caption fields; expression)
    assert_eq!(call(&mut s, cat, "DataCaptions"), "P7 Entries - TGT1 \u{b7} Target One|P7 Expr Caption - Hello");
    // LastError.Rethrow() raises the last error again (message and code)
    assert_eq!(call(&mut s, cat, "RethrowLastError"), "DB:RecordNotFound|The P7 Item does not");
    // a new record takes no value from a non-key filter in a private filter group
    assert_eq!(call(&mut s, cat, "NewRecordFromFilters"), "0|3");
    // a page variable's OnValidate that changes Rec: the page saves the record
    assert_eq!(call(&mut s, cat, "PageVarSavesRecord"), "7");
    // BindSubscription on a bound instance: false when the result is used, an error otherwise
    assert_eq!(call(&mut s, cat, "BindTwiceTrapped"), "No|The object is already bound to the event subscriptions.");
    // DateTime standard formats 1 (seconds and <Second dec.>) and 3 (seconds)
    assert_eq!(call(&mut s, cat, "DateTimeStdFormats"), "01/27/28 1:14 PM|01/27/28 1:14:15.12 PM|01/27/28 1:14:15 PM|01/27/28 1:14:15 PM");
    // FieldType values format as their member names
    assert_eq!(call(&mut s, cat, "FieldTypeNames"), "Boolean,Boolean,Date,Yes");
    // division by zero: BC's DivideByZeroException code and text
    assert_eq!(call(&mut s, cat, "DivideByZeroCode"), "AL:DivideByZero|Attempted to divide by zero.");
    // a row of a two-dimensional array as a var / value array parameter
    assert_eq!(call(&mut s, cat, "ArrayRows"), "5,6,107,0,1");
    // TestPage.FILTER.SetFilter replaces the page's group-0 filter even with group 2 active
    assert_eq!(call(&mut s, cat, "TestPageFilterGroup"), "FG-A|FG-B|FG-B");
    // a TestPage saving its new record: OnInsert's xRec is the record with the values entered
    assert_eq!(call(&mut s, cat, "PageInsertXRec"), "Dynamic|Dynamic");
    // a control's Visible expression with a quoted field name containing a dot
    assert_eq!(call(&mut s, cat, "VisibleWithDottedField"), "NoYes");
    // a Variant DateTime passed to a Date parameter / assigned to a Date is its date
    assert_eq!(call(&mut s, cat, "VariantDateParam"), "2028-01-27|2028-01-27");
    // Rec.Field::Value / xRec.Field::Value with an enum named like the field
    assert_eq!(call(&mut s, cat, "RecScopes"), "Charge (Item)|YesYesCharge (Item)");
    // TaskScheduler: ready tasks run at Sleep, not-ready ones after SetTaskReady, cancelled ones
    // never; a failing task is rolled back and its failure codeunit runs with GetLastErrorText
    assert_eq!(call(&mut s, cat, "TaskSchedulerRuns"), "YesYesNo|1NoYes|2|2Yes11");
    // Find(Which): each of '=', '>', '<' in the order given, the first record found
    assert_eq!(call(&mut s, cat, "FindDirections"), "YesA1YesA3YesA3YesA1YesA5NoA0YesA1NoA2");
    assert_eq!(
        call(&mut s, cat, "CollectionTraps"),
        "NoNoNoNo|An invalid argument was passed to a 'List' data type method.|Yes|YesNo|The given key was not present in the dictionary.|573"
    );
    assert_eq!(call(&mut s, cat, "DateFormulaFields"), "NoYes1M+CM|Yes|.");
    // a binding ends with the scope that owns its instance (BindLocal, BindAndClear) or Unbind;
    // Clear detaches the variable but the binding lives until the clearing frame ends, or until the
    // frame binds another instance of the codeunit (BindSubscription(Sub3) ends it here)
    assert_eq!(call(&mut s, cat, "BindLifetime"), "01110");
    // Clear(X); BindSubscription(X) in one frame: one subscriber (the cleared instance's binding ends)
    assert_eq!(call(&mut s, cat, "ClearThenRebind"), "11");
    assert_eq!(
        call(&mut s, cat, "CollectionErrors"),
        "The given key was not present in the dictionary.|An item with the same key has already been added.|An invalid argument was passed to a 'List' data type method."
    );
    assert_eq!(call(&mut s, cat, "DateFormulas"), "+1D|<+1D>|2026-11-08|1M-D5|No");
    assert_eq!(call(&mut s, cat, "TestFieldCodes"), "TestField|TestField");
    assert_eq!(call(&mut s, cat, "InterfaceDefaults"), "impl|hello x");
    assert_eq!(call(&mut s, cat, "PictureAbbreviations"), "0007|1.50|012|10:11:12Yes");
    assert_eq!(call(&mut s, cat, "ListOverloads"), "27");
    assert_eq!(call(&mut s, cat, "NumberPictures"), "08|   8|**|1,234,567|-1.50|1.23|7.00|2015-03");
    assert_eq!(call(&mut s, cat, "JsonSecrets"), "YesYesx");
    assert_eq!(call(&mut s, cat, "RecordLinks"), "NoYesYesYes|1|0No");
    assert_eq!(call(&mut s, cat, "EnumFieldRefOptions"), "Red,Green|Red Caption,Green Caption|Green Caption");
    assert_eq!(call(&mut s, cat, "OptionArray"), "YesUsagePriceSchPrice");
    assert_eq!(call(&mut s, cat, "PageExtGlobals"), "Yes");
    assert_eq!(call(&mut s, cat, "ErrorInfos"), "boom|Yes|T|v|No");
    assert_eq!(call(&mut s, cat, "CollectTwo"), "2NoYessecond");
    assert_eq!(call(&mut s, cat, "CollectRaisedAtEnd"), "collected one");
    s.rollback();
}

/// MinValue / MaxValue: checked for values entered on a page, not for Validate from code.
#[test]
fn field_ranges_on_input() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let h = s.new_record(50700, false);
    let (min_qty, pct) = {
        let b = h.borrow();
        (b.tdef.field("MinQty").unwrap(), b.tdef.field("Pct").unwrap())
    };
    let msg = |r: Result<(), Flow>| match r {
        Err(Flow::Err(e)) => e.msg,
        Ok(()) => "ok".into(),
        Err(_) => "flow".into(),
    };
    let dec = |s: &str| Val::Dec(s.parse().unwrap());
    assert_eq!(msg(s.ui_validate(&h, min_qty, Some(dec("-2.5")))), "The value must be greater than or equal to 0. Value: -2.5.");
    assert_eq!(msg(s.ui_validate(&h, pct, Some(Val::Int(101)))), "The value must be less than or equal to 100. Value: 101.");
    assert_eq!(msg(s.ui_validate(&h, pct, Some(Val::Int(-1)))), "ok");
    assert_eq!(msg(s.rec_validate(&h, min_qty, Some(dec("-2.5")), 0)), "ok");
    assert_eq!(h.borrow().get_val(min_qty).to_text(), "-2.5");
}

/// A field changed through the UI: CurrFieldNo is the field's number while it validates.
#[test]
fn curr_field_no_from_ui() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let h = s.new_record(50700, false);
    let fi = h.borrow().tdef.field("Amount").unwrap();
    if let Err(Flow::Err(e)) = s.ui_validate(&h, fi, Some(Val::Int(3))) {
        panic!("{}", e.msg);
    }
    let seen = h.borrow().tdef.field("SeenCurrField").unwrap();
    assert_eq!(h.borrow().get_val(seen).to_text(), "2");
    assert_eq!(s.curr_field_no(&h), 0);
}

/// Download / Upload answer like BC's web client under the test runner; without a client they fail.
#[test]
fn file_transfer() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    s.gui_allowed = true;
    assert_eq!(call(&mut s, cat, "FileTransfer"), "YesYesreport.txtNoNo");
    s.gui_allowed = false;
    let ci = cat.code.iter().position(|c| c.name == "P7 Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find("FileTransfer")[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Err(Flow::Err(e)) => assert!(e.msg.contains("session has no client"), "{}", e.msg),
        _ => panic!("expected the no-client error"),
    }
}
