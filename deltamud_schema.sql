-- DeltaMUD Database Schema
-- Based on analysis of dbinterface.c

USE deltamud;

-- Main player table with 83 columns as defined in NUM_PLAYER_MAIN_ROW_ELEMENTS
CREATE TABLE IF NOT EXISTS player_main (
    idnum INT PRIMARY KEY,
    name VARCHAR(30) NOT NULL UNIQUE,
    description TEXT,
    title VARCHAR(80),
    sex TINYINT,
    class TINYINT,
    race TINYINT,
    deity TINYINT,
    level TINYINT,
    hometown INT,
    birth BIGINT,
    played BIGINT,
    weight INT,
    height INT,
    pwd VARCHAR(50),
    last_logon BIGINT,
    host VARCHAR(80),
    
    -- Character stats
    act BIGINT,
    str TINYINT,
    str_add TINYINT,
    intel TINYINT,
    wis TINYINT,
    dex TINYINT,
    con TINYINT,
    cha TINYINT,
    
    -- Gameplay stats
    hit INT,
    max_hit INT,
    mana INT,
    max_mana INT,
    move INT,
    max_move INT,
    gold INT,
    bank_gold INT,
    exp BIGINT,
    power INT,
    mpower INT,
    defense INT,
    mdefense INT,
    technique INT,
    
    -- Player specials
    PADDING0 INT,
    talks1 INT,
    talks2 INT,
    talks3 INT,
    wimp_level INT,
    freeze_level TINYINT,
    invis_level TINYINT,
    load_room INT,
    pref BIGINT,
    bad_pws TINYINT,
    cond1 TINYINT,
    cond2 TINYINT,
    cond3 TINYINT,
    death_timer INT,
    citizen INT,
    training TINYINT,
    newbie TINYINT,
    arena INT,
    spells_to_learn INT,
    questpoints INT,
    nextquest INT,
    countdown INT,
    questobj INT,
    questmob INT,
    recall_level TINYINT,
    retreat_level TINYINT,
    trust TINYINT,
    bail_amt INT,
    wins INT,
    losses INT,
    pref2 BIGINT,
    godcmds1 BIGINT,
    godcmds2 BIGINT,
    godcmds3 BIGINT,
    godcmds4 BIGINT,
    clan INT,
    clan_rank TINYINT,
    mapx INT,
    mapy INT,
    buildmodezone INT,
    buildmoderoom INT,
    tloadroom INT,
    
    -- Character specials
    alignment INT,
    affected_by BIGINT,
    
    INDEX idx_name (name),
    INDEX idx_level (level)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Player affects table
CREATE TABLE IF NOT EXISTS player_affects (
    idnum INT NOT NULL,
    type INT,
    duration INT,
    modifier INT,
    location TINYINT,
    bitvector BIGINT,
    INDEX idx_idnum (idnum),
    FOREIGN KEY (idnum) REFERENCES player_main(idnum) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Player skills table
CREATE TABLE IF NOT EXISTS player_skills (
    idnum INT NOT NULL,
    skill INT,
    learned TINYINT,
    INDEX idx_idnum (idnum),
    INDEX idx_skill (skill),
    FOREIGN KEY (idnum) REFERENCES player_main(idnum) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Add a test immortal character for initial login
INSERT INTO player_main (idnum, name, pwd, level, sex, class, race, deity, hometown, birth, played, last_logon, 
                        hit, max_hit, mana, max_mana, move, max_move, gold, exp, 
                        str, intel, wis, dex, con, cha, alignment, load_room)
VALUES (1, 'Admin', 'XXXXXXXXXXXX', 60, 1, 0, 0, 0, 0, UNIX_TIMESTAMP(), 0, UNIX_TIMESTAMP(),
        500, 500, 100, 100, 100, 100, 50000, 0,
        18, 18, 18, 18, 18, 18, 0, 0);