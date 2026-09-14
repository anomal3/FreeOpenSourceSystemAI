// Чужой взгляд на сборку .NET для фазы N1.
//
// Печатает канонический текст о сборке: сколько строк в каждой таблице и где
// она лежит, типы, поля, методы с сигнатурами и байтами IL, обработчики
// исключений, ссылки, атрибуты и строки программы. `cargo xtask clr-check`
// печатает то же самое нашим разбором (crates/clr-meta) и сверяет построчно.
//
// Формат обязан совпадать с xtask/src/clrcheck.rs до символа. Всё, что не
// печатный ASCII, выводится как \uXXXX по единицам UTF-16: так сравнение не
// зависит ни от кодировки консоли, ни от того, как каждая сторона понимает
// одиночные половинки суррогатных пар.

using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using System.Reflection.PortableExecutable;
using System.Text;

if (args.Length != 1)
{
    Console.Error.WriteLine("usage: clrdump <assembly>");
    return 2;
}

using var stream = File.OpenRead(args[0]);
using var pe = new PEReader(stream);
if (!pe.HasMetadata)
{
    Console.Error.WriteLine("not a managed assembly");
    return 1;
}

var md = pe.GetMetadataReader();
using var output = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false), 1 << 16);

void Line(string text)
{
    output.Write(text);
    output.Write('\n');
}

var cor = pe.PEHeaders.CorHeader!;
Line($"runtime {cor.MajorRuntimeVersion}.{cor.MinorRuntimeVersion} flags 0x{(uint)cor.Flags:x8} entry 0x{(uint)cor.EntryPointTokenOrRelativeVirtualAddress:x8}");
Line($"metadata {Escape(md.MetadataVersion)}");

for (int table = 0; table <= 0x2C; table++)
{
    var index = (TableIndex)table;
    int rows = md.GetTableRowCount(index);
    if (rows == 0)
    {
        continue;
    }
    Line($"table {table:x2} rows {rows} size {md.GetTableRowSize(index)} offset {md.GetTableMetadataOffset(index)}");
}

foreach (var handle in md.TypeReferences)
{
    var type = md.GetTypeReference(handle);
    Line($"typeref {Row(handle)} scope={Tok(type.ResolutionScope)} ns={Escape(md.GetString(type.Namespace))} name={Escape(md.GetString(type.Name))}");
}

foreach (var handle in md.TypeDefinitions)
{
    var type = md.GetTypeDefinition(handle);
    var fields = type.GetFields();
    var methods = type.GetMethods();
    string fieldSpan = $"{fields.Count}@{(fields.Count > 0 ? MetadataTokens.GetRowNumber(fields.First()) : 0)}";
    string methodSpan = $"{methods.Count}@{(methods.Count > 0 ? MetadataTokens.GetRowNumber(methods.First()) : 0)}";
    Line($"typedef {Row(handle)} flags=0x{(uint)type.Attributes:x8} ns={Escape(md.GetString(type.Namespace))} name={Escape(md.GetString(type.Name))} extends={Tok(type.BaseType)} fields={fieldSpan} methods={methodSpan}");
}

foreach (var handle in md.FieldDefinitions)
{
    var field = md.GetFieldDefinition(handle);
    Line($"field {Row(handle)} flags=0x{(ushort)field.Attributes:x4} name={Escape(md.GetString(field.Name))} sig={Hex(md.GetBlobBytes(field.Signature))}");
}

foreach (var handle in md.MethodDefinitions)
{
    var method = md.GetMethodDefinition(handle);
    int rva = method.RelativeVirtualAddress;
    Line($"method {Row(handle)} rva=0x{rva:x8} impl=0x{(ushort)method.ImplAttributes:x4} flags=0x{(ushort)method.Attributes:x4} name={Escape(md.GetString(method.Name))} sig={Hex(md.GetBlobBytes(method.Signature))} params={method.GetParameters().Count}");
    if (rva == 0)
    {
        continue;
    }
    var body = pe.GetMethodBody(rva);
    int locals = body.LocalSignature.IsNil ? 0 : MetadataTokens.GetToken(body.LocalSignature);
    Line($"body {Row(handle)} maxstack={body.MaxStack} init={(body.LocalVariablesInitialized ? 1 : 0)} locals=0x{locals:x8} il={Hex(body.GetILBytes()!)}");
    foreach (var region in body.ExceptionRegions)
    {
        uint extra = region.Kind switch
        {
            ExceptionRegionKind.Catch => (uint)MetadataTokens.GetToken(region.CatchType),
            ExceptionRegionKind.Filter => (uint)region.FilterOffset,
            _ => 0,
        };
        Line($"eh {Row(handle)} kind={(int)region.Kind} try={region.TryOffset}+{region.TryLength} handler={region.HandlerOffset}+{region.HandlerLength} extra=0x{extra:x8}");
    }
}

foreach (var handle in md.MemberReferences)
{
    var member = md.GetMemberReference(handle);
    Line($"memberref {Row(handle)} parent={Tok(member.Parent)} name={Escape(md.GetString(member.Name))} sig={Hex(md.GetBlobBytes(member.Signature))}");
}

foreach (var handle in md.AssemblyReferences)
{
    var reference = md.GetAssemblyReference(handle);
    Line($"asmref {Row(handle)} name={Escape(md.GetString(reference.Name))} version={reference.Version} flags=0x{(uint)reference.Flags:x8} key={Hex(md.GetBlobBytes(reference.PublicKeyOrToken))} culture={Escape(md.GetString(reference.Culture))}");
}

foreach (var handle in md.CustomAttributes)
{
    var attribute = md.GetCustomAttribute(handle);
    Line($"attr {Row(handle)} parent={Tok(attribute.Parent)} ctor={Tok(attribute.Constructor)} value={Hex(md.GetBlobBytes(attribute.Value))}");
}

if (md.GetHeapSize(HeapIndex.UserString) > 1)
{
    for (var handle = MetadataTokens.UserStringHandle(1); !handle.IsNil; handle = md.GetNextHandle(handle))
    {
        string text = md.GetUserString(handle);
        if (text.Length > 0)
        {
            Line($"us {MetadataTokens.GetHeapOffset(handle)} {Escape(text)}");
        }
    }
}

return 0;

static int Row(EntityHandle handle) => MetadataTokens.GetRowNumber(handle);

static string Tok(EntityHandle handle)
{
    if (handle.IsNil)
    {
        return "00:0";
    }
    int token = MetadataTokens.GetToken(handle);
    return $"{(token >> 24) & 0xFF:x2}:{token & 0xFFFFFF}";
}

static string Hex(byte[] bytes) => Convert.ToHexString(bytes).ToLowerInvariant();

static string Escape(string text)
{
    var builder = new StringBuilder(text.Length);
    foreach (char unit in text)
    {
        if (unit >= 0x20 && unit < 0x7F && unit != '\\')
        {
            builder.Append(unit);
        }
        else
        {
            builder.Append($"\\u{(int)unit:x4}");
        }
    }
    return builder.ToString();
}
