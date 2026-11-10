using System;
using Microsoft.Extensions.Logging.EventLog;

namespace Microsoft.Extensions.Logging.EventLog
{
    public class EventLogSettings
    {
        public string? LogName { get; set; }
        public string? SourceName { get; set; }
        public string? MachineName { get; set; }
        public Func<string, LogLevel, bool>? Filter { get; set; }
    }

    public sealed class EventLogLoggerProvider : ILoggerProvider, ISupportExternalScope
    {
        public EventLogLoggerProvider() { }
        public EventLogLoggerProvider(EventLogSettings settings) { }
        public ILogger CreateLogger(string name) => NullLogger.Instance;
        public void SetScopeProvider(IExternalScopeProvider scopeProvider) { }
        public void Dispose() { }
    }

    internal sealed class NullLogger : ILogger
    {
        public static readonly NullLogger Instance = new();
        public IDisposable? BeginScope<TState>(TState state) where TState : notnull => null;
        public bool IsEnabled(LogLevel logLevel) => false;
        public void Log<TState>(LogLevel logLevel, EventId eventId, TState state, Exception? exception, Func<TState, Exception?, string> formatter) { }
    }
}

namespace Microsoft.Extensions.Logging
{
    public static class EventLoggerFactoryExtensions
    {
        public static ILoggingBuilder AddEventLog(this ILoggingBuilder builder) => builder;
        public static ILoggingBuilder AddEventLog(this ILoggingBuilder builder, EventLogSettings settings) => builder;
        public static ILoggingBuilder AddEventLog(this ILoggingBuilder builder, Action<EventLogSettings> configure) => builder;
    }
}
