CREATE TABLE IF NOT EXISTS player_skills (
    idnum INT NOT NULL, skill INT, learned TINYINT,
    INDEX idx_idnum (idnum), INDEX idx_skill (skill)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4
