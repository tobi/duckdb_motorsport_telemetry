-- Convert one full Pi/Cosworth PDS recording into the common subset of the
-- IER simulator's MoTeC schema, including an LDX lap-marker sidecar.
--
-- Usage:
--   TELEMETRY_FROM=/path/to/run.pds \
--   TELEMETRY_TO=/path/to/run.ld \
--   duckdb -unsigned -f scripts/pds_to_sim_motec.sql

LOAD motorsport_telemetry;

SET VARIABLE from_file = CASE
    WHEN getenv('TELEMETRY_FROM') = ''
    THEN error('set TELEMETRY_FROM to the input .pds file')
    ELSE getenv('TELEMETRY_FROM')
END;
SET VARIABLE to_file = CASE
    WHEN getenv('TELEMETRY_TO') = ''
    THEN error('set TELEMETRY_TO to the output .ld file')
    ELSE getenv('TELEMETRY_TO')
END;

SELECT *
FROM write_telemetry(
    getvariable('from_file'),
    getvariable('to_file'),

    -- Standard DuckDB lists, not a project-specific mapping language.
    -- Each row is [source channel, output channel, output unit]. Empty units
    -- preserve unitless counters/flags. All physical conversions come from
    -- telemetry_units(); no scale constants are duplicated here.
    -- Keep every source channel not explicitly renamed or excluded below.
    -- The mapping augments the full recording; it is not a 22-channel projection.
    include_unmapped := true,
    exclude_channels := [
        'X_FL_DAMPER', 'X_FR_DAMPER',
        'X_RL_DAMPER', 'X_RR_DAMPER',
        'X_FrC_Damper', 'X_RrC_Damper'
    ],
    channel_mapping := [
        ['Speed_Ref',      'Ground Speed',       'm/s'],
        ['RPM',            'Engine RPM',         'rpm'],
        ['gear_pos',       'Gear',               ''],
        ['STEER',          'Steering Angle',     'deg'],
        ['P_F_BRAKE',      'Brake Pressure F',   'psi'],
        ['P_R_BRAKE',      'Brake Pressure R',   'psi'],
        ['T_Brake_FL',     'Brake Temp FL',      '°C'],
        ['T_Brake_FR',     'Brake Temp FR',      '°C'],
        ['T_Brake_RL',     'Brake Temp RL',      '°C'],
        ['T_Brake_RR',     'Brake Temp RR',      '°C'],
        ['P_Tyre_FL',      'Tire Pressure FL',   'psi'],
        ['P_Tyre_FR',      'Tire Pressure FR',   'psi'],
        ['P_Tyre_RL',      'Tire Pressure RL',   'psi'],
        ['P_Tyre_RR',      'Tire Pressure RR',   'psi'],
        ['T_Tyre_FL',      'Tire Temp Core FL',  '°C'],
        ['T_Tyre_FR',      'Tire Temp Core FR',  '°C'],
        ['T_Tyre_RL',      'Tire Temp Core RL',  '°C'],
        ['T_Tyre_RR',      'Tire Temp Core RR',  '°C'],
        ['Fuel Remaining', 'Fuel Level',         'l'],
        ['I_ACCEL_LONG',   'G Force Long',       'G'],
        ['tcsActive',      'TC Active',          '']
    ],

    -- Each row is [left source, right source, output channel, output unit].
    -- Source clocks and units must match or the export fails.
    sum_channels := [
        ['X_FL_DAMPER', 'X_FR_DAMPER', 'Damper Travel HF', 'mm']
    ],

    -- Session identity encoded by this Pi/Cosworth naming convention. Keep
    -- unknown driver names as their recorded initials; DriverID and related
    -- channels are retained above rather than inventing an identity mapping.
    driver := regexp_extract(getvariable('from_file'),
        '_Run[0-9]+_([^_]+)_MQ', 1),
    vehicle := regexp_extract(getvariable('from_file'),
        '(MQ[^/]+ #[0-9]+)\\.pds$', 1),
    vehicle_number := regexp_extract(getvariable('from_file'), '#([0-9]+)\\.pds$', 1),
    venue := 'Sebring',
    event := 'Sebring Test 2026',
    session := regexp_extract(getvariable('from_file'), '_(CT[0-9]+)_Run', 1),
    comment := regexp_extract(getvariable('from_file'), '_(Run[0-9]+)_', 1),
    date := strftime(strptime(regexp_extract(getvariable('from_file'),
        '/([0-9]{6})[0-9]{6}_', 1), '%y%m%d'), '%d/%m/%Y'),
    time := strftime(strptime(regexp_extract(getvariable('from_file'),
        '/[0-9]{6}([0-9]{6})_', 1), '%H%M%S'), '%H:%M:%S')
);
