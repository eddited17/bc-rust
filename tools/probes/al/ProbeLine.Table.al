// Output of probe runs: JSON lines (one per cell plus one meta line per probe and context), stored in chunks.
// The runner reads column Data with SQL inside the container, so no large HTTP response is needed.
table 50400 "BCR Probe Line"
{
    Caption = 'BCR Probe Line';
    DataClassification = SystemMetadata;
    DataPerCompany = false;

    fields
    {
        field(1; "Run Id"; Code[50]) { }
        field(2; Chunk; Integer) { }
        field(3; "Probe Id"; Text[100]) { }
        field(4; Lines; Integer) { }
        field(5; Data; Blob) { Compressed = false; }
        field(6; "Created At"; DateTime) { }
    }

    keys
    {
        key(PK; "Run Id", Chunk) { Clustered = true; }
    }
}

// Binary outputs of probe cells (e.g. one workbook per case), written by "BCR Probe Log".AddFile. The runner
// copies them with SQL to files in the container's shared folder; nothing goes through HTTP.
table 50402 "BCR Probe File"
{
    Caption = 'BCR Probe File';
    DataClassification = SystemMetadata;
    DataPerCompany = false;

    fields
    {
        field(1; "Run Id"; Code[50]) { }
        field(2; Seq; Integer) { }
        field(3; "Probe Id"; Text[100]) { }
        field(4; Cell; Integer) { }
        field(5; Name; Text[250]) { }
        field(6; Data; Blob) { Compressed = false; }
        field(7; Size; Integer) { }
    }

    keys
    {
        key(PK; "Run Id", Seq) { Clustered = true; }
    }
}
